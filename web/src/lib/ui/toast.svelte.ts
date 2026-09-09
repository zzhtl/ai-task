// 轻量通知。
//
// 触发执行、保存任务、删除记录这类动作以前是静默成功的：按钮按下去，
// 页面跳走或者列表刷新，人得自己判断"成了没有"。这里给一个明确的回执。
// 错误也走这里——页面顶部的红条留给"整页都加载不出来"那种。

export type ToastKind = 'ok' | 'bad' | 'info';

export interface Toast {
  id: number;
  kind: ToastKind;
  text: string;
}

let items = $state<Toast[]>([]);
let seq = 0;

function dismiss(id: number) {
  items = items.filter((t) => t.id !== id);
}

function push(kind: ToastKind, text: string, ttlMs: number): number {
  const id = ++seq;
  items = [...items, { id, kind, text }];
  setTimeout(() => dismiss(id), ttlMs);
  return id;
}

export const toasts = {
  get items() {
    return items;
  },
  dismiss
};

export function toast(text: string) {
  push('ok', text, 3200);
}
export function toastInfo(text: string) {
  push('info', text, 3200);
}
/** 错误多停一会儿：人得有时间把原因读完。 */
export function toastError(text: string) {
  push('bad', text, 7000);
}
