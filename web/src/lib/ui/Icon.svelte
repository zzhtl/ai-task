<script lang="ts">
  /**
   * 图标。名字来自 `icons.ts`，写错的名字**编译期就报错**——
   * 之前内联 path 的写法里，抄错一个字符只会画出一团看不懂的线，没有任何提示。
   *
   * 尺寸和描边继承 app.css 里 `button svg` 那套规则，这里只在需要时覆盖。
   */
  import { ICONS, type IconName } from './icons';

  let {
    name,
    size,
    label
  }: {
    name: IconName;
    /** 像素。不给就继承上下文（按钮里是 15px）。 */
    size?: number;
    /** 给了就是有意义的图形，会被读屏念出来；不给就是纯装饰。 */
    label?: string;
  } = $props();
</script>

<svg
  viewBox="0 0 24 24"
  width={size}
  height={size}
  role={label ? 'img' : undefined}
  aria-label={label}
  aria-hidden={label ? undefined : 'true'}>{@html ICONS[name]}</svg
>
