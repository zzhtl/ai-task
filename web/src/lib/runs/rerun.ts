// 用同样的输入再跑一次。执行记录表和首页的"最近失败"共用。
import { getRun, newIdempotencyKey, triggerRun } from '$api/runs';

/**
 * 重跑一次执行，返回新 run 的 id。
 *
 * 列表项里没有 inputs（列表只选摘要列），得先取一下详情；影子执行重跑出来还是影子执行。
 */
export async function rerun(run: { id: string; task_id: string; dry_run: boolean }): Promise<string> {
  const detail = await getRun(run.id);
  const next = await triggerRun(
    run.task_id,
    { dry_run: run.dry_run, inputs: detail.inputs ?? undefined },
    newIdempotencyKey()
  );
  return next.id;
}
