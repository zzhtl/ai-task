// 设计 token 的对比度门禁。
//
// 浅色主题的状态色曾经和暗色主题共用同一组 hex，理由写在 app.css 里：
// "状态的含义不该随主题变"。意图是对的，实现把**含义**和**明度**混为一谈了——
// #35c07f 在白底上只有 2.3:1，读不出来的绿色不传达任何含义。
//
// 修一次不够：下一个人调色板时会再踩一遍。所以把规则写成测试，而不是写成注释。

import { describe, expect, test } from 'bun:test';

const CSS = await Bun.file(new URL('../../app.css', import.meta.url)).text();

/** 从一个 `:root` 块里抽出全部 `--x: 值;`，值原样保留。 */
function declarations(selector: string): Record<string, string> {
  const start = CSS.indexOf(selector);
  if (start < 0) throw new Error(`app.css 里找不到 ${selector}`);
  const open = CSS.indexOf('{', start);
  const close = CSS.indexOf('\n}', open);
  const block = CSS.slice(open, close).replace(/\/\*[\s\S]*?\*\//g, '');
  const out: Record<string, string> = {};
  for (const [, name, value] of block.matchAll(/(--[\w-]+):\s*([^;]+);/g)) out[name] = value.trim();
  return out;
}

/**
 * 把 `var()` 链解到底。
 *
 * 只认 `#rrggbb` 的旧版测试看不见 `--st-ai: var(--st-ai)` 这种写法——它是个无效值，
 * 暗色下 AI 步骤的序号没颜色、编排图的 AI 节点没有描边，而浏览器和构建都不报错。
 * 自引用、循环、指向没声明的名字，一律在这里失败。
 */
function resolve(theme: Record<string, string>, name: string, seen: string[] = []): string {
  if (seen.includes(name)) throw new Error(`循环引用：${[...seen, name].join(' → ')}`);
  const value = theme[name];
  if (value === undefined) throw new Error(`未声明：${[...seen, name].join(' → ')}`);
  const ref = value.match(/^var\((--[\w-]+)\)$/);
  return ref ? resolve(theme, ref[1], [...seen, name]) : value;
}

/** 解开之后是 `#rrggbb` 的那些 token——能算对比度的就是它们。 */
function hexTokens(theme: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const name of Object.keys(theme)) {
    const value = resolve(theme, name);
    if (/^#[0-9a-fA-F]{6}$/.test(value)) out[name] = value.toLowerCase();
  }
  return out;
}

const DARK_RAW = declarations(':root {');
const LIGHT_RAW = { ...DARK_RAW, ...declarations(":root[data-theme='light'] {") };
const DARK = hexTokens(DARK_RAW);
const LIGHT = hexTokens(LIGHT_RAW);

function channel(c: number): number {
  const v = c / 255;
  return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
}

function luminance(hex: string): number {
  const n = parseInt(hex.slice(1), 16);
  return (
    0.2126 * channel((n >> 16) & 255) +
    0.7152 * channel((n >> 8) & 255) +
    0.0722 * channel(n & 255)
  );
}

export function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

/** 前景 token 必须在**每一种**它可能落上去的表面上都达标，所以取最差的那个。 */
function worst(theme: Record<string, string>, token: string): { ratio: number; on: string } {
  const surfaces = ['--bg', '--surface-1', '--surface-2', '--surface-3'];
  let ratio = Infinity;
  let on = '';
  for (const s of surfaces) {
    const r = contrast(theme[token], theme[s]);
    if (r < ratio) {
      ratio = r;
      on = s;
    }
  }
  return { ratio, on };
}

/** 当文字用的 token：AA 正文 4.5:1。 */
const AS_TEXT = [
  '--fg',
  '--fg-dim',
  '--fg-faint',
  '--accent-fg',
  // StatusBadge 把这五个当文字色用
  '--st-running',
  '--st-succeeded',
  '--st-failed',
  '--st-timeout',
  '--st-resource',
  // 步骤序号
  '--st-ai'
];

/** 只当圆点/描边用的 token：AA 非文字 3:1。 */
const AS_GRAPHIC = ['--st-queued', '--st-cancelled', '--st-skipped', '--st-awaiting', '--accent'];

describe.each([
  ['暗色', DARK],
  ['浅色', LIGHT]
])('%s主题', (_name, theme) => {
  test.each(AS_TEXT)('%s 当文字用，最差表面上也要 ≥4.5:1', (token) => {
    const { ratio, on } = worst(theme, token);
    expect(`${token} ${theme[token]} on ${on}: ${ratio.toFixed(2)}`).toBe(
      `${token} ${theme[token]} on ${on}: ${Math.max(ratio, 4.5).toFixed(2)}`
    );
  });

  test.each(AS_GRAPHIC)('%s 当图形用，最差表面上也要 ≥3:1', (token) => {
    const { ratio, on } = worst(theme, token);
    expect(`${token} ${theme[token]} on ${on}: ${ratio.toFixed(2)}`).toBe(
      `${token} ${theme[token]} on ${on}: ${Math.max(ratio, 3).toFixed(2)}`
    );
  });
});

const TONES = ['ok', 'bad', 'warn', 'info', 'violet', 'neutral', 'accent'];

describe.each([
  ['暗色', DARK],
  ['浅色', LIGHT]
])('%s主题的徽标与按钮', (_name, theme) => {
  // 徽标是"淡底 + 同色调文字"：文字除了落在各种表面上，还落在自己的淡底上
  test.each(TONES)('--%s-fg 在自己的淡底和所有表面上都 ≥4.5:1', (tone) => {
    const fg = theme[`--${tone}-fg`];
    const bg = theme[`--${tone}-bg`];
    expect(fg).toBeDefined();
    expect(bg).toBeDefined();
    const onOwn = contrast(fg, bg);
    const { ratio } = worst(theme, `--${tone}-fg`);
    expect(Math.min(onOwn, ratio)).toBeGreaterThanOrEqual(4.5);
  });

  test('主按钮上的字在强调色上 ≥4.5:1', () => {
    expect(contrast(theme['--accent-on'], theme['--accent'])).toBeGreaterThanOrEqual(4.5);
  });
});

describe.each([
  ['暗色', DARK_RAW],
  ['浅色', LIGHT_RAW]
])('%s主题的 var() 链', (_name, theme) => {
  test.each(Object.keys(theme))('%s 能解开：没有自引用、循环或未声明的名字', (token) => {
    expect(() => resolve(theme, token)).not.toThrow();
  });
});

test('组件里用到的每个 var(--x) 都在 app.css 里声明过', async () => {
  // 写错一个名字，浏览器只会安静地回退成继承值或初始值
  const root = new URL('../../', import.meta.url).pathname;
  const used = new Set<string>();
  for await (const file of new Bun.Glob('**/*.{svelte,css}').scan({ cwd: root, absolute: true })) {
    const text = await Bun.file(file).text();
    for (const [, name] of text.matchAll(/var\(\s*(--[\w-]+)/g)) used.add(name);
  }
  const missing = [...used].filter((name) => !(name in LIGHT_RAW)).sort();
  expect(missing).toEqual([]);
});

test('浅色主题必须自己定义全部状态色，不能继承暗色的', () => {
  const own = hexTokens({ ...DARK_RAW, ...declarations(":root[data-theme='light'] {") });
  const lightOnly = declarations(":root[data-theme='light'] {");
  // 继承暗色的 hex 正是最初那个 bug：同一个绿在白底上只有 2.3:1。
  for (const token of ['--st-running', '--st-succeeded', '--st-failed', '--st-timeout']) {
    expect(lightOnly[token]).toBeDefined();
    expect(own[token]).not.toBe(DARK[token]);
  }
});
