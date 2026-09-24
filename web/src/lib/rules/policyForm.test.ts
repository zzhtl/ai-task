import { describe, expect, test } from 'bun:test';
import type { Rule } from '$api/models';
import {
  describeMatcher,
  emptyPolicyForm,
  policyBody,
  policyFormFromRule,
  policyProblems
} from './policyForm';

function rule(spec: Record<string, unknown>, extra: Partial<Rule> = {}): Rule {
  return {
    id: 'r1',
    name: 'x',
    kind: 'policy',
    scope: 'global',
    spec,
    priority: 100,
    enabled: true,
    created_at: '2026-09-01T00:00:00Z',
    ...extra
  };
}

/**
 * 服务端存下来的真实形状：`serde_json::to_value(PolicyRuleBody)`。
 * `scope` 总是带着，`tool` / `arg` 为 None 时不出现，`any_of` 总是数组。
 */
const SPECS: Array<[string, Record<string, unknown>, Partial<Rule>]> = [
  [
    '不限工具 + 多条模式（含 equals）',
    {
      effect: 'deny',
      reason: '破坏性命令一律拒绝',
      scope: { host_tags: [], task_ids: [] },
      match: {
        arg: 'command',
        any_of: [{ contains: 'rm -rf' }, { equals: 'reboot' }, { regex: '\\bmkfs\\b' }]
      }
    },
    { priority: 300 }
  ],
  [
    '只限工具、没有 arg、没有模式',
    {
      effect: 'deny',
      reason: '默认拒绝 Bash',
      scope: { host_tags: [], task_ids: [] },
      match: { tool: 'Bash', any_of: [] }
    },
    { priority: 0 }
  ],
  [
    '主机 tag 与任务范围',
    {
      effect: 'ask',
      reason: '生产机写文件要人确认',
      scope: { host_tags: ['prod', 'db'], task_ids: ['01a0d17a-dc0d-77e0-9320-baacdf6fd4b7'] },
      match: { tool: 'remote_write', any_of: [{ glob: '/etc/*' }] }
    },
    { priority: 200, scope: 'task' }
  ]
];

describe('硬策略表单往返', () => {
  test.each(SPECS)('%s：原样打开、原样保存，语义不变', (_name, spec, extra) => {
    const original = rule(spec, extra);
    const body: Record<string, unknown> = policyBody(policyFormFromRule(original));
    expect(body).toEqual({
      kind: 'policy',
      effect: spec.effect,
      reason: spec.reason,
      priority: original.priority,
      global: original.scope === 'global',
      scope: spec.scope,
      match: spec.match
    });
  });

  test('「任意工具」不能被存成 tool: ""——那会什么都匹配不上', () => {
    const form = policyFormFromRule(rule(SPECS[0][1]));
    expect(form.tool).toBe('');
    expect('tool' in policyBody(form).match).toBe(false);
  });

  test('没有 arg 的规则不能被塞进 arg: "command"', () => {
    const form = policyFormFromRule(rule(SPECS[1][1]));
    expect(form.arg).toBe('');
    expect('arg' in policyBody(form).match).toBe(false);
  });

  test('还没填的模式行不发出去，主机 tag 去空白去重', () => {
    const form = emptyPolicyForm();
    form.patterns = [
      { kind: 'regex', value: '^ls\\b' },
      { kind: 'equals', value: '' }
    ];
    form.hostTags = [' prod ', 'prod', '', 'db'];
    const body = policyBody(form);
    expect(body.match.any_of).toEqual([{ regex: '^ls\\b' }]);
    expect(body.scope.host_tags).toEqual(['prod', 'db']);
  });
});

describe('保存前的检查', () => {
  test('新建时要名字，原因不能空', () => {
    const form = emptyPolicyForm();
    expect(policyProblems(form, true)).toEqual(['还没起名字', '原因不能为空：它会回给模型']);
    form.name = 'n';
    form.reason = 'r';
    form.patterns = [{ kind: 'regex', value: 'x' }];
    expect(policyProblems(form, true)).toEqual([]);
  });

  test('不限工具又没有模式：会命中所有调用，要拦下来', () => {
    const form = { ...emptyPolicyForm(), name: 'n', reason: 'r', tool: '', patterns: [] };
    expect(policyProblems(form, false)).toEqual(['既不限工具又没有模式，会命中所有工具调用']);
  });
});

describe('匹配条件的一行描述', () => {
  test('不限工具时说"任意工具"，四种模式都认得', () => {
    expect(describeMatcher(SPECS[0][1])).toBe(
      '任意工具.command ~ 包含 rm -rf | 完全相等 reboot | 正则 \\bmkfs\\b'
    );
    expect(describeMatcher(SPECS[1][1])).toBe('Bash 的所有调用');
  });
});
