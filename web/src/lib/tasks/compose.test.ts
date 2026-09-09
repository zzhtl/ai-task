import { describe, expect, test } from 'bun:test';
import { DEFAULT_MODEL, fromSpec, newStep, toSpec, type Composition } from './compose';
import type { DagSpec } from '$api/types/DagSpec';

/**
 * 一份"界面管不到"的编排：output_schema、非默认 retry、limits、max_turns、
 * 收得更紧的 tools、shell 的 working_dir、approval 的 on_timeout。
 *
 * 界面上没有任何地方能编辑这些，所以也没有任何地方会提示它们被删掉了。
 */
const RICH: DagSpec = {
  nodes: [
    {
      key: 'step-1',
      name: '巡检',
      config: {
        kind: 'ai',
        prompt: '看看 prod',
        executor: 'claude_code',
        model: 'claude-haiku-4-5-20251001',
        // 比"只读"档更紧：档位有三档，这个清单不属于任何一档
        tools: ['Read'],
        max_turns: 6,
        effort: 'low'
      },
      output_schema: { type: 'object', required: ['ok'] },
      retry: { max_attempts: 3, backoff_ms: 5000, backoff_factor: 3, feed_error_to_model: false },
      on_failure: 'continue',
      timeout_s: 300
    },
    {
      key: 'step-2',
      name: '修',
      config: { kind: 'shell', command: 'systemctl restart nginx', working_dir: '/srv' },
      limits: { memory_mib: 64, cpu_percent: 50, pids_max: 8 },
      inputs: { 上一步: { from: 'node', node: 'step-1', path: '$' } },
      retry: { max_attempts: 1, backoff_ms: 2000, backoff_factor: 2, feed_error_to_model: true },
      on_failure: 'fail_fast',
      timeout_s: 120
    },
    {
      key: 'step-3',
      name: '确认重启',
      config: { kind: 'approval', title: '确认重启', timeout_s: 900, on_timeout: 'approve' },
      inputs: { 上一步: { from: 'node', node: 'step-2', path: '$' } },
      retry: { max_attempts: 1, backoff_ms: 2000, backoff_factor: 2, feed_error_to_model: true },
      on_failure: 'fail_fast',
      timeout_s: 900
    }
  ],
  edges: [
    { from: 'step-1', to: 'step-2', when: { op: 'on_success' } },
    { from: 'step-2', to: 'step-3', when: { op: 'on_success' } }
  ],
  budget_usd: '0.500000'
} as unknown as DagSpec;

describe('步骤编辑器的往返', () => {
  test('打开再保存，一个字段都不能少', () => {
    // 界面上已经没有 YAML 兜底了：这里丢掉的东西，用户没有任何办法拿回来
    const comp = fromSpec(RICH);
    expect(comp).not.toBeNull();
    expect(toSpec(comp as Composition)).toEqual(RICH);
  });

  test('审批步骤以卡片上那句话为准，画布标签跟着它走', () => {
    // 两个标题、一个输入框，必须挑一个当主。被改掉画布标签只是画布上换个字；
    // 被改掉审批文案是让审批人看到错的东西。
    const two = structuredClone(RICH) as unknown as {
      nodes: Array<{ name: string; config: { title?: string } }>;
    };
    two.nodes[2].name = '第三步';
    two.nodes[2].config.title = '确认要重启 nginx 吗';

    const comp = fromSpec(two as unknown as DagSpec) as Composition;
    expect(comp.steps[2].title).toBe('确认要重启 nginx 吗');

    const back = toSpec(comp) as unknown as {
      nodes: Array<{ name: string; config: { title: string } }>;
    };
    expect(back.nodes[2].config.title).toBe('确认要重启 nginx 吗');
    expect(back.nodes[2].name).toBe('确认要重启 nginx 吗');
  });

  test('收得更紧的工具白名单不会被档位放宽', () => {
    // ["Read"] 会被显示成"只读"档，但只读档是三个工具。
    // 按档位写回去 = 悄悄给了 Glob 和 Grep。
    const comp = fromSpec(RICH) as Composition;
    expect(comp.steps[0].tools).toEqual(['Read']);
    const back = toSpec(comp) as unknown as { nodes: Array<{ config: { tools: string[] } }> };
    expect(back.nodes[0].config.tools).toEqual(['Read']);
  });

  test('改了标题只动标题', () => {
    const comp = fromSpec(RICH) as Composition;
    comp.steps[0].title = '换个名字';
    const back = toSpec(comp) as unknown as {
      nodes: Array<{ name: string; output_schema?: unknown; retry: { max_attempts: number } }>;
    };
    expect(back.nodes[0].name).toBe('换个名字');
    expect(back.nodes[0].output_schema).toEqual({ type: 'object', required: ['ok'] });
    expect(back.nodes[0].retry.max_attempts).toBe(3);
  });

  test('换步骤类型时旧配置不能留下来', () => {
    // ai 的 prompt/model 留在 shell 节点上，后端 deny_unknown_fields 会 422
    const comp = fromSpec(RICH) as Composition;
    comp.steps[0].kind = 'shell';
    comp.steps[0].body = 'df -h';
    const back = toSpec(comp) as unknown as { nodes: Array<{ config: Record<string, unknown> }> };
    expect(back.nodes[0].config).toEqual({ kind: 'shell', command: 'df -h' });
  });

  test('引用跟着步骤走，不跟着位置走', () => {
    // 第 3 步看第 1 步；把第 1、2 步对调之后，它该指向"原来那一步的新位置"，
    // 而不是继续指着 step-1
    const comp = fromSpec(RICH) as Composition;
    const first = comp.steps[0].uid;
    comp.steps[2].sees = [first];
    [comp.steps[0], comp.steps[1]] = [comp.steps[1], comp.steps[0]];

    const back = toSpec(comp) as unknown as {
      nodes: Array<{ inputs?: Record<string, { node: string }> }>;
    };
    const refs = Object.values(back.nodes[2].inputs ?? {}).map((r) => r.node);
    expect(refs).toContain('step-2');
    expect(refs).not.toContain('step-1');
  });

  test('把一步挪到它的数据来源前面，那条引用会被丢掉而不是变成循环', () => {
    const comp = fromSpec(RICH) as Composition;
    [comp.steps[0], comp.steps[1]] = [comp.steps[1], comp.steps[0]];
    const back = toSpec(comp) as unknown as { nodes: Array<{ inputs?: unknown }> };
    // 原来的第 2 步现在排在第 1 位，它的来源跑到后面去了
    expect(back.nodes[0].inputs).toBeUndefined();
  });

  test('一步能同时看见好几步的结果', () => {
    // 真实流程：最后一步回写 Jira 要同时用到第 1 步的单号和第 3 步的修复结果。
    // 只能看上一步的话，中间每一步都得把前面的信息原样再抄一遍
    const steps = ['读 Jira', '连机器', '复现并修复', '回写 Jira'].map((title, i) => ({
      ...newStep('ai'),
      title,
      body: title,
      sees: [] as string[],
      uid: `u${i}`
    }));
    steps[3].sees = [steps[0].uid, steps[2].uid];

    const back = toSpec({ steps, budgetUsd: null }) as unknown as {
      nodes: Array<{ inputs?: Record<string, { node: string }> }>;
    };
    const last = back.nodes[3].inputs ?? {};
    expect(Object.values(last).map((r) => r.node).sort()).toEqual(['step-1', 'step-3']);
    // 名字会成为提示词里的小标题，得能看出是哪一步
    expect(Object.keys(last)).toContain('第1步：读 Jira');
  });

  test('新建的 AI 步骤把模型钉死，不留给 CLI 决定', () => {
    // runs.fingerprint 里含模型。不指定的话"同指纹"可能指的是两个不同模型，
    // 漂移检测就从告警变成了噪音。
    const spec = toSpec({
      steps: [{ ...newStep('ai'), body: '看看 prod' }],
      budgetUsd: null
    }) as unknown as { nodes: Array<{ config: { model?: string } }> };
    expect(spec.nodes[0].config.model).toBe(DEFAULT_MODEL);
    expect(DEFAULT_MODEL).toBe('claude-sonnet-5');
  });

  test('老任务没指定模型时不被静默替换', () => {
    // 悄悄给一个正在跑的任务换模型，比让它继续不指定更糟
    const bare = {
      nodes: [{ key: 'step-1', config: { kind: 'ai', prompt: 'x', executor: 'claude_code' } }],
      edges: []
    } as unknown as DagSpec;
    const comp = fromSpec(bare) as Composition;
    expect(comp.steps[0].model).toBeNull();
    const back = toSpec(comp) as unknown as { nodes: Array<{ config: { model?: string } }> };
    expect(back.nodes[0].config).not.toHaveProperty('model');
  });

  test('关掉目标机执行时 cli 键要真的删掉', () => {
    // 留着旧值就是"界面上关了、实际还开着"
    const comp: Composition = {
      steps: [{ ...newStep('ai'), body: 'x', runner: { kind: 'host_cli', cli: 'codex' } }],
      budgetUsd: null
    };
    const on = toSpec(comp) as unknown as { nodes: Array<{ config: Record<string, unknown> }> };
    expect(on.nodes[0].config.cli).toBe('codex');

    comp.steps[0].runner = { kind: 'center' };
    comp.steps[0].raw = on.nodes[0] as unknown as Record<string, unknown>;
    const off = toSpec(comp) as unknown as { nodes: Array<{ config: Record<string, unknown> }> };
    expect(off.nodes[0].config).not.toHaveProperty('cli');
    expect(off.nodes[0].config.executor).toBe('claude_code');
  });
});
