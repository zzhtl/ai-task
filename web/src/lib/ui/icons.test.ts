// 图标表的完整性。
//
// `<Icon name="…">` 的名字是联合类型，svelte-check 能抓到拼错——但只在 .svelte
// 的模板里能抓到。表里堆着没人用的图标则完全没有信号，时间一长就变成一坨死数据。
// 这两头都在这里锁住。

import { describe, expect, test } from 'bun:test';
import { ICONS } from './icons';

const SRC = new URL('../../', import.meta.url).pathname;

async function sources(): Promise<string[]> {
  const glob = new Bun.Glob('**/*.svelte');
  const out: string[] = [];
  for await (const file of glob.scan({ cwd: SRC, absolute: true })) {
    out.push(await Bun.file(file).text());
  }
  return out;
}

const ALL = (await sources()).join('\n');
const used = new Set([...ALL.matchAll(/<Icon\s+name="([^"]+)"/g)].map((m) => m[1]));
// Shell 的导航图标是数据驱动的：`icon: 'activity'`，不是模板里的字面量
for (const m of ALL.matchAll(/icon: '([a-z-]+)'/g)) used.add(m[1]);

describe('图标表', () => {
  test('模板里引用的每个名字都在表里', () => {
    const missing = [...used].filter((name) => !(name in ICONS));
    expect(missing).toEqual([]);
  });

  test('表里没有没人用的图标', () => {
    // 留着不用的图标不会报错，只会慢慢积成一坨没人敢删的数据
    const orphans = Object.keys(ICONS).filter((name) => !used.has(name));
    expect(orphans).toEqual([]);
  });

  test('每个图标都是能画出来的 SVG 片段', () => {
    for (const [name, body] of Object.entries(ICONS)) {
      expect(body, name).toMatch(/^<(path|circle|rect|g)\b/);
      // 标签闭合了；没闭合的话整个 svg 会被浏览器丢掉，而且不报错
      expect(body, name).toMatch(/\/>$/);
    }
  });
});
