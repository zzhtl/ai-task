// 编辑器保存前要说清楚的问题。
//
// 拦不住的留给后端的 422，但那是保存之后的事；这里的每一条都是"现在就看得出来、
// 不用等服务器"的，而且能指到具体哪一步。
import type { Composition } from './compose';

export interface Problem {
  /** 属于哪一步；整个任务层面的问题是 `null`。 */
  stepUid: string | null;
  /** `error` 拦住保存；`warn` 只提醒。 */
  level: 'error' | 'warn';
  text: string;
}

/** 后端 UsdMicros 收的写法：非负、最多 6 位小数。 */
const MONEY = /^\d+(\.\d{1,6})?$/;

export function problemsOf(name: string, comp: Composition): Problem[] {
  const out: Problem[] = [];
  if (name.trim() === '') out.push({ stepUid: null, level: 'error', text: '还没起名字' });

  comp.steps.forEach((step, i) => {
    const at = `第 ${i + 1} 步`;
    const push = (level: Problem['level'], text: string) => out.push({ stepUid: step.uid, level, text });

    if (step.kind !== 'approval' && step.body.trim() === '') push('error', `${at}还没写要做什么`);

    if (step.kind === 'ai') {
      if (step.budgetUsd !== null && !MONEY.test(step.budgetUsd.trim())) push('error', `${at}的花费上限不是金额`);
    }
    if (!Number.isInteger(step.maxAttempts) || step.maxAttempts < 1 || step.maxAttempts > 5) {
      push('error', `${at}的尝试次数要在 1 到 5 之间`);
    }
    if (step.kind === 'ai' && step.maxTurns !== null) {
      if (!Number.isInteger(step.maxTurns) || step.maxTurns < 1 || step.maxTurns > 200) {
        push('error', `${at}的轮数要在 1 到 200 之间`);
      }
    }

    // 保存时这些引用会被丢掉（看后面的步骤在一条直线上就是循环），不说的话人不会发现
    const positions = step.sees.map((uid) => comp.steps.findIndex((s) => s.uid === uid));
    for (const target of positions) {
      if (target >= i) {
        push('warn', `${at}勾选的「${comp.steps[target].title || `第 ${target + 1} 步`}」排到了它后面，看不到了`);
      }
    }
    if (i > 0 && !positions.some((target) => target >= 0 && target < i)) {
      push('warn', `${at}看不到前面任何一步的结果`);
    }
  });

  if (comp.budgetUsd !== null && comp.budgetUsd.trim() !== '' && !MONEY.test(comp.budgetUsd.trim())) {
    out.push({ stepUid: null, level: 'error', text: '整个任务的花费上限不是金额' });
  }
  return out;
}
