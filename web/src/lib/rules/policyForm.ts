// 硬策略的编辑表单 ⇄ 接口报文。
//
// 之前表单只认"一个工具 + 一个参数 + 一条模式"，编辑时先把规则压成这个形状再整条写回：
// 「任意工具」被存成 `tool: ""`（什么都匹配不上，护栏悄悄失效）；没有 `arg` 的规则被塞进
// `arg: "command"`；多条模式只剩第一条；`equals` 被改成 `regex`（匹配变宽）；`scope` 从不
// 发送，服务端按默认值把主机 tag / 任务范围清空。改一个错别字，规则的语义就变了，界面上
// 什么都看不出来。
//
// 这里的规矩：**空值就是"不设"，不是空串**；表单不编辑的 `task_ids` 原样带回。
// 服务端的 ToolMatcher / RuleScope 都是 deny_unknown_fields，所以报文里只出现它们认识的字段。

import type { Rule } from '$api/models';

export type PatternKind = 'regex' | 'glob' | 'contains' | 'equals';
export type PolicyEffect = 'allow' | 'deny' | 'ask';

export const PATTERN_KINDS: Array<{ id: PatternKind; label: string }> = [
  { id: 'regex', label: '正则' },
  { id: 'glob', label: '通配' },
  { id: 'contains', label: '包含' },
  { id: 'equals', label: '完全相等' }
];

export interface PatternRow {
  kind: PatternKind;
  value: string;
}

export interface PolicyForm {
  name: string;
  /** 空串 = 任意工具，报文里不带 `tool`。 */
  tool: string;
  /** 空串 = 匹配整个输入的 JSON 文本，报文里不带 `arg`。 */
  arg: string;
  /** 空列表 = 只要工具对上就命中。 */
  patterns: PatternRow[];
  effect: PolicyEffect;
  reason: string;
  priority: number;
  global: boolean;
  /** 只对带这些 tag 的主机生效；空 = 不限。 */
  hostTags: string[];
  /** 界面不编辑（只能通过接口设），原样带回。 */
  taskIds: string[];
}

interface RawMatcher {
  tool?: string | null;
  arg?: string | null;
  any_of?: Array<Record<string, string>>;
}
interface RawScope {
  host_tags?: string[];
  task_ids?: string[];
}

const KINDS = new Set<string>(PATTERN_KINDS.map((k) => k.id));

export function emptyPolicyForm(): PolicyForm {
  return {
    name: '',
    tool: 'Bash',
    arg: 'command',
    patterns: [{ kind: 'regex', value: '' }],
    effect: 'deny',
    reason: '',
    priority: 100,
    global: true,
    hostTags: [],
    taskIds: []
  };
}

/** 一条模式 `{regex: "..."}` → 表单行。认不出的种类返回 null，由调用方决定怎么报。 */
function toRow(pattern: Record<string, string>): PatternRow | null {
  const [kind, value] = Object.entries(pattern)[0] ?? [];
  if (!kind || !KINDS.has(kind) || typeof value !== 'string') return null;
  return { kind: kind as PatternKind, value };
}

export function policyFormFromRule(rule: Rule): PolicyForm {
  const match = (rule.spec.match ?? {}) as RawMatcher;
  const scope = (rule.spec.scope ?? {}) as RawScope;
  return {
    name: rule.name,
    tool: match.tool ?? '',
    arg: match.arg ?? '',
    patterns: (match.any_of ?? []).map(toRow).filter((r): r is PatternRow => r !== null),
    effect: String(rule.spec.effect ?? 'deny') as PolicyEffect,
    reason: String(rule.spec.reason ?? ''),
    priority: rule.priority,
    global: rule.scope === 'global',
    hostTags: [...(scope.host_tags ?? [])],
    taskIds: [...(scope.task_ids ?? [])]
  };
}

/** 主机 tag：去空白、去空、去重，保持输入顺序。 */
export function normalizeTags(tags: string[]): string[] {
  const out: string[] = [];
  for (const t of tags.map((x) => x.trim())) if (t && !out.includes(t)) out.push(t);
  return out;
}

/**
 * 表单 → `PUT /rules/{id}` 的报文（新建时调用方再补 `name`）。
 *
 * 值为空的模式行丢掉：那是"加了一行还没填"，发出去会是一条匹配空串的模式。
 */
export function policyBody(form: PolicyForm) {
  const match: RawMatcher = {};
  const tool = form.tool.trim();
  const arg = form.arg.trim();
  if (tool) match.tool = tool;
  if (arg) match.arg = arg;
  match.any_of = form.patterns
    .filter((p) => p.value !== '')
    .map((p) => ({ [p.kind]: p.value }));
  return {
    kind: 'policy' as const,
    effect: form.effect,
    reason: form.reason,
    priority: form.priority,
    global: form.global,
    scope: { host_tags: normalizeTags(form.hostTags), task_ids: [...form.taskIds] },
    match
  };
}

/** 保存前挡掉的问题。服务端还会再编一次正则，这里只拦明显的空缺。 */
export function policyProblems(form: PolicyForm, creating: boolean): string[] {
  const out: string[] = [];
  if (creating && !form.name.trim()) out.push('还没起名字');
  if (!form.reason.trim()) out.push('原因不能为空：它会回给模型');
  if (!form.tool.trim() && form.patterns.every((p) => p.value === '')) {
    out.push('既不限工具又没有模式，会命中所有工具调用');
  }
  return out;
}

/** 匹配条件压成一行给人扫。 */
export function describeMatcher(spec: Record<string, unknown>): string {
  const m = (spec.match ?? {}) as RawMatcher;
  const head = m.tool ? m.tool : '任意工具';
  const target = m.arg ? `${head}.${m.arg}` : head;
  const rows = (m.any_of ?? []).map(toRow).filter((r): r is PatternRow => r !== null);
  if (!rows.length) return `${head} 的所有调用`;
  const label = (k: PatternKind) => PATTERN_KINDS.find((x) => x.id === k)?.label ?? k;
  return `${target} ~ ${rows.map((r) => `${label(r.kind)} ${r.value}`).join(' | ')}`;
}
