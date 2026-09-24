// 文本框随内容长高。提示词动辄十几行，固定 4 行的框只能在里面滚着改。
//
// 用法：`<textarea {@attach autogrow}>`。内容被程序改掉时（换模板、撤销）元素是新建的，
// 挂上时量一次就够；打字时跟着 input 量。
export function autogrow(el: HTMLTextAreaElement): () => void {
  const fit = () => {
    el.style.height = 'auto';
    // 加上边框，不然最后一行会被裁掉一点
    el.style.height = `${el.scrollHeight + el.offsetHeight - el.clientHeight}px`;
  };
  fit();
  el.addEventListener('input', fit);
  return () => el.removeEventListener('input', fit);
}
