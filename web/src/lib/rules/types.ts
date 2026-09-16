// 三个页签共用的形状。
//
// 拆分之前这些东西散在一个 614 行的文件里：19 个平铺的 $state 表单变量、
// 三套近乎相同的表格、一个 closeForm 要负责把全部三组字段清干净。
// 加一个字段就得同时改四处，漏一处不会报错，只会在下一次打开表单时
// 留着上一次的残值。

import type { Rule } from '$api/models';

/** 页面提供的共享动作。错误处理、toast、重新加载都在它里面，页签不用各写一遍。 */
export type Act = (run: () => Promise<unknown>, done?: string) => Promise<void>;

/** 三个页签都要的那几样。 */
export interface TabShared {
  busy: boolean;
  /** 表单开着。新增和编辑共用同一个弹层。 */
  adding: boolean;
  /** 正在改的那条；`null` 表示新增。 */
  editing: Rule | null;
  /** 后端 422 按字段拆出来的错误。 */
  fieldErr: Record<string, string>;
  act: Act;
  onclose: () => void;
}
