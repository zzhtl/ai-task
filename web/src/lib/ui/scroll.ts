// 滚到某个元素。
//
// 单独拎出来是因为有两件事容易写漏：
//   * `behavior: 'smooth'` 要尊重系统的"减弱动效"设置——全站其它动效都尊重了，
//     只有这里硬写 smooth 说不过去；
//   * 目标可能不在（数据还没到、节点被过滤掉），这时候什么都不做，别抛。

export function scrollToNode(key: string): void {
  if (typeof document === 'undefined') return;
  scrollToElement(document.querySelector(`[data-node="${CSS.escape(key)}"]`));
}

export function scrollToElement(target: Element | null): void {
  if (!target) return;
  const reduce =
    typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
  target.scrollIntoView({ behavior: reduce ? 'auto' : 'smooth', block: 'center' });
}
