// 把「按顺序执行的一串步骤」变成一份 DagSpec。
//
// 大多数任务是线性的：先看看情况 → 再判断 → 再动手。为这种任务写 YAML 是
// 纯粹的负担，但"只给一个输入框"又表达不了顺序——所以中间形态是**有序步骤**：
// 每步说清楚做什么、在哪做、谁来做，前后自动串起来。
//
// 关键在于**每一步都看得见上一步的结果**：不串起来的话，三个步骤就只是三个
// 互不相干的任务，"按顺序"这件事就没有意义。
//
// 反过来也要能走：线性的单链 DAG 还原回步骤列表。界面上**只有这一种编排方式**
// ——没有 YAML 逃生舱了，所以还原不回来就是真的改不了，必须诚实地说出来。

import type { DagSpec } from '$api/types/DagSpec';

/** spec 里的输入引用。只关心是不是指向别的节点。 */
interface RawRef {
  from?: string;
  node?: string;
}

/** 一步做什么。 */
export type StepKind = 'ai' | 'shell' | 'approval';

/** 谁来执行 AI 步骤。 */
export type Runner =
  /** 中心的 Claude Code。目标机什么都不用装，工具调用回中心过策略。 */
  | { kind: 'center' }
  /** 目标机上的 CLI。需要那台机器装了它；它的内置工具**不经过策略层**。 */
  | { kind: 'host_cli'; cli: string };

/**
 * 这一步的 AI 能动到什么。
 *
 * 默认只读**不是保守**，是因为「忘了配」不该等于「能改文件」。但反过来，
 * 界面上不给这个开关就等于让人写完提示词才发现模型说「Bash 被禁用了」
 * ——那次执行的钱已经花掉了。
 */
export type Reach = 'read_only' | 'run_commands' | 'edit_files';

export const REACH_TOOLS: Record<Reach, string[]> = {
  read_only: ['Read', 'Glob', 'Grep'],
  run_commands: ['Read', 'Glob', 'Grep', 'Bash'],
  edit_files: ['Read', 'Glob', 'Grep', 'Bash', 'Write', 'Edit']
};

export interface Step {
  /** 仅用于列表渲染的稳定 key，不进 spec。 */
  uid: string;
  kind: StepKind;
  /** 这一步叫什么。会成为节点 key 的来源，也显示在画布上。 */
  title: string;
  /** AI 步骤是提示词；shell 步骤是命令行；审批步骤忽略。 */
  body: string;
  /** 在哪台机器上。`null` = 本机（中心）。 */
  hostId: string | null;
  /** 只对 AI 步骤有意义。 */
  runner: Runner;
  /**
   * 用哪个模型。新建的步骤一律钉死到 [`DEFAULT_MODEL`]。
   *
   * `null` 表示"没指定，交给 CLI 挑"——只有打开老任务时才可能是这个值。
   * 不指定的代价不是省事：`runs.fingerprint` 里含模型，模型不定的话
   * "同指纹"可能指的是两个不同模型，漂移检测就变成了噪音。
   */
  model: string | null;
  /**
   * 这一步的 AI 能用哪些工具。[`Reach`] 只是它在界面上的显示形态。
   *
   * 存的是**真实的工具清单**而不是档位：档位只有三档，把 `["Read"]` 这种
   * 收得更紧的白名单归到"只读"档再按档位写回去，等于**悄悄放宽了权限**。
   */
  tools: string[];
  timeoutS: number;
  skills: string[];
  /**
   * 这一步能看到**哪几步**的结果，按步骤 uid 记（uid 在拖动排序时不变，序号会变）。
   *
   * 只串相邻两步是不够的：一个真实流程里，"最后一步回写 Jira"要同时用到
   * 第 1 步拿到的单号和第 3 步的修复结果。只能看上一步的话，中间每一步都得
   * 把前面的信息原样再抄一遍——抄丢了没人发现。
   */
  sees: string[];
  /**
   * 原始节点。保存时以它为底，只覆盖编辑器真正管的那几个字段。
   *
   * 编辑器不认识的东西——`output_schema`、`retry`、`limits`、`max_turns`、
   * shell 的 `working_dir`——不这样带着走的话，**改一次标题就会把它们静默抹掉**，
   * 而界面上没有任何地方提示过它们存在。以前还有 YAML 模式兜底，现在没有了。
   */
  raw?: Record<string, unknown>;
}

export interface Composition {
  steps: Step[];
  /** 整个任务的花费上限（美元）。`null` 表示不设。 */
  budgetUsd: string | null;
}

/** 工具白名单 → 权限档位。认不出来就按最保守的算。 */
export function reachOf(tools: string[]): Reach {
  if (tools.includes('Write') || tools.includes('Edit')) return 'edit_files';
  if (tools.includes('Bash')) return 'run_commands';
  return 'read_only';
}

/**
 * 新步骤的模型。
 *
 * Sonnet 5 是日常够用的那一档：Opus 贵得多，Haiku 在需要判断力的活儿上
 * 会给出看起来对、其实不对的答案——而这个系统里的任务是无人值守跑的，
 * 没人在旁边发现它答错了。
 */
export const DEFAULT_MODEL = 'claude-sonnet-5';

let counter = 0;
export function newStep(kind: StepKind = 'ai'): Step {
  counter += 1;
  return {
    uid: `s${counter}-${Date.now()}`,
    kind,
    title: kind === 'ai' ? '让 AI 做一件事' : kind === 'shell' ? '运行一条命令' : '人工确认',
    body: '',
    hostId: null,
    runner: { kind: 'center' },
    model: DEFAULT_MODEL,
    tools: [...REACH_TOOLS.read_only],
    timeoutS: kind === 'approval' ? 900 : 600,
    skills: [],
    sees: []
  };
}

export const DEFAULTS: Composition = {
  steps: [newStep('ai')],
  budgetUsd: '1.000000'
};

/**
 * 步骤 → 节点 key。
 *
 * key 会进 JSONPath 和文件路径，字符集被后端限死在 `[A-Za-z0-9_-]`。
 * 标题是中文的居多，所以不拿标题当 key——用序号，稳定且永远合法。
 */
function keyOf(index: number): string {
  return `step-${index + 1}`;
}

/** `step-3` → 2。认不出来给 -1，调用方据此忽略这条引用。 */
function indexOfKey(key: string | undefined): number {
  const at = Number(key?.replace(/^step-/, ''));
  return Number.isFinite(at) ? at - 1 : -1;
}

/** 编辑器管不到、但节点必须有的东西的默认值。只在原节点没有时才填。 */
const DEFAULT_RETRY = {
  max_attempts: 1,
  backoff_ms: 2000,
  backoff_factor: 2,
  feed_error_to_model: true
};
const DEFAULT_SHELL_LIMITS = { memory_mib: 1024, cpu_percent: 200, pids_max: 128 };
const CONFIG_KIND: Record<StepKind, string> = { ai: 'ai', shell: 'shell', approval: 'approval' };

/**
 * 步骤列表 → DagSpec。相邻两步之间一条 on_success 边。
 *
 * **以原始节点为底，只覆盖编辑器真正管的字段。**直接从零构造节点的话，
 * `output_schema` / `retry` / `limits` / `max_turns` 这些编辑器不认识的东西
 * 会在保存时消失得无声无息。
 */
export function toSpec(comp: Composition): DagSpec {
  const nodes = comp.steps.map((step, i) => {
    const raw = step.raw ?? {};
    const node: Record<string, unknown> = { ...raw };
    node.key = keyOf(i);
    node.name = step.title || keyOf(i);
    node.timeout_s = step.timeoutS;
    node.retry ??= DEFAULT_RETRY;
    node.on_failure ??= 'fail_fast';

    // 换了步骤类型就不能再沿用旧 config：ai 的字段留在 shell 节点上是垃圾，
    // 而且后端 deny_unknown_fields 会直接 422
    const rawConfig = (raw.config as Record<string, unknown> | undefined) ?? {};
    const keep = rawConfig.kind === CONFIG_KIND[step.kind] ? { ...rawConfig } : {};

    if (step.kind === 'approval') {
      const config: Record<string, unknown> = {
        ...keep,
        kind: 'approval',
        title: step.title || '需要确认',
        timeout_s: step.timeoutS
      };
      config.on_timeout ??= 'deny';
      node.config = config;
    } else if (step.kind === 'shell') {
      node.config = { ...keep, kind: 'shell', command: step.body };
      node.limits ??= DEFAULT_SHELL_LIMITS;
    } else {
      const config: Record<string, unknown> = {
        ...keep,
        kind: 'ai',
        prompt: step.body,
        executor: step.runner.kind === 'host_cli' ? 'host_cli' : 'claude_code'
      };
      config.max_turns ??= 30;
      // 关掉某个开关时必须真的删掉那个键，留着旧值就是"界面上关了、实际还开着"
      if (step.runner.kind === 'host_cli') config.cli = step.runner.cli;
      else delete config.cli;
      if (step.model) config.model = step.model;
      else delete config.model;
      if (step.tools.length) config.tools = [...step.tools];
      else delete config.tools;
      if (step.skills.length) config.skills = [...step.skills];
      else delete config.skills;
      node.config = config;
    }

    if (step.hostId) node.host = { on: 'host', host_id: step.hostId };
    else delete node.host;

    // 把选中的那几步的结果喂给这一步。**不串起来的话，"按顺序"就没有意义**
    // ——几个步骤会变成几个互不相干的任务。
    //
    // 指向节点的输入整个由编辑器接管（先清空再按 `sees` 重建），免得步骤被
    // 挪动之后还指着原来的上游；literal / run_input 那些编辑器管不到的原样留着。
    const rawInputs = (raw.inputs as Record<string, RawRef> | undefined) ?? {};
    const inputs: Record<string, unknown> = Object.fromEntries(
      Object.entries(rawInputs).filter(([, ref]) => ref?.from !== 'node')
    );
    for (const uid of step.sees) {
      const at = comp.steps.findIndex((s) => s.uid === uid);
      // 只能看更早的步骤。看后面的在一条直线上就是循环依赖
      if (at < 0 || at >= i) continue;
      const key = keyOf(at);
      // 名字会成为提示词里的小标题。原来就指着同一个节点的输入沿用原名，
      // 免得只改个标题就把模型看到的措辞换掉了
      const existing = Object.entries(rawInputs).find(
        ([, ref]) => ref?.from === 'node' && ref.node === key
      );
      const name = existing ? existing[0] : `第${at + 1}步：${comp.steps[at].title || key}`;
      inputs[name] = { from: 'node', node: key, path: '$' };
    }
    if (Object.keys(inputs).length) node.inputs = inputs;
    else delete node.inputs;

    return node;
  });

  const edges = comp.steps.slice(1).map((_, i) => ({
    from: keyOf(i),
    to: keyOf(i + 1),
    when: { op: 'on_success' }
  }));

  const spec: Record<string, unknown> = { nodes, edges };
  if (comp.budgetUsd) spec.budget_usd = comp.budgetUsd;
  return spec as unknown as DagSpec;
}

/**
 * DagSpec → 步骤列表。表示不了就返回 `null`。
 *
 * 调用方据此显示"这个编排步骤列表表示不了"，**不是**降级到某种原始编辑器：
 * 界面上已经没有那个东西了。
 *
 * 能表示的是**一条链**：n 个节点、n-1 条 on_success 边、首尾相接。
 * 分支、并行、map、条件边都是结构，步骤列表里没有它们的位置——
 * **硬塞进去只会让人以为自己编辑的是全部**，然后保存时悄悄丢掉一半。
 */
export function fromSpec(spec: DagSpec): Composition | null {
  const nodes = (spec.nodes ?? []) as unknown as Array<Record<string, unknown>>;
  const edges = (spec.edges ?? []) as unknown as Array<Record<string, unknown>>;
  if (nodes.length === 0) return null;
  if (edges.length !== nodes.length - 1) return null;

  // 必须首尾相接，且全是 on_success
  for (let i = 0; i < edges.length; i++) {
    const e = edges[i];
    const when = e.when as { op?: string } | undefined;
    if (when && when.op !== 'on_success') return null;
    if (e.from !== nodes[i].key || e.to !== nodes[i + 1].key) return null;
  }

  const steps: Step[] = [];
  for (const node of nodes) {
    const config = node.config as Record<string, unknown> | undefined;
    if (!config) return null;
    const host = node.host as { on?: string; host_id?: string } | undefined;
    // 按 tag 选主机在步骤列表里表示不了
    if (host && host.on !== 'host') return null;

    const kind = config.kind as string;
    if (kind !== 'ai' && kind !== 'shell' && kind !== 'approval') return null;
    // api executor 是另一回事（自建 Messages 循环），步骤列表不覆盖
    if (kind === 'ai' && config.executor === 'api') return null;

    counter += 1;
    steps.push({
      uid: `s${counter}-${node.key as string}`,
      kind,
      // 审批节点有两个标题：node.name 是画布标签，config.title 是**审批卡片上
      // 给人看的那句话**。编辑器只有一个标题框，所以以后者为准——被覆盖掉
      // 画布标签只是画布上换个字，被覆盖掉审批文案是让审批人看到错的东西。
      title:
        (kind === 'approval' ? (config.title as string) : undefined) ??
        (node.name as string) ??
        (config.title as string) ??
        (node.key as string),
      body: kind === 'shell' ? ((config.command as string) ?? '') : ((config.prompt as string) ?? ''),
      hostId: host?.host_id ?? null,
      runner:
        config.executor === 'host_cli'
          ? { kind: 'host_cli', cli: (config.cli as string) ?? 'claude' }
          : { kind: 'center' },
      model: (config.model as string) ?? null,
      tools: (config.tools as string[]) ?? [],
      timeoutS: (node.timeout_s as number) ?? 600,
      skills: (config.skills as string[]) ?? [],
      // 指向节点的输入 → 看得见哪几步。steps 是按顺序建的，被引用的更早的
      // 步骤此时已经在数组里了
      sees: Object.values((node.inputs as Record<string, RawRef> | undefined) ?? {})
        .filter((ref) => ref?.from === 'node')
        .map((ref) => steps[indexOfKey(ref.node)]?.uid)
        .filter((uid): uid is string => uid !== undefined),
      raw: node
    });
  }

  return {
    steps,
    budgetUsd: ((spec as unknown as Record<string, unknown>).budget_usd as string) ?? null
  };
}
