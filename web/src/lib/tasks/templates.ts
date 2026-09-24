// 新建任务的起点。
//
// 空白之外的模板是 JSON 夹具：Rust 那边逐个跑 DAG 校验
// （crates/ai-task-core/tests/templates.rs），这边的测试保证它们能原样进出步骤编辑器。
import type { DagSpec } from '$api/types/DagSpec';
import inspect from './templates/inspect.json';
import diagnoseFix from './templates/diagnose-fix.json';

export interface TaskTemplate {
  id: string;
  name: string;
  hint: string;
  /** `null` = 空白：一个空的 AI 步骤。 */
  spec: DagSpec | null;
}

export const TEMPLATES: TaskTemplate[] = [
  { id: 'blank', name: '空白', hint: '从一个 AI 步骤开始，自己往下加', spec: null },
  {
    id: 'inspect',
    name: '只读巡检',
    hint: '跑一条命令收集指标，AI 判断有没有异常。什么都不改',
    spec: inspect as unknown as DagSpec
  },
  {
    id: 'diagnose-fix',
    name: '诊断 → 人工确认 → 修复',
    hint: 'AI 先只读诊断，有人点头之后才动手修',
    spec: diagnoseFix as unknown as DagSpec
  }
];
