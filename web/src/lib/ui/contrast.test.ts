// 设计 token 的对比度门禁。
//
// 浅色主题的状态色曾经和暗色主题共用同一组 hex，理由写在 app.css 里：
// "状态的含义不该随主题变"。意图是对的，实现把**含义**和**明度**混为一谈了——
// #35c07f 在白底上只有 2.3:1，读不出来的绿色不传达任何含义。
//
// 修一次不够：下一个人调色板时会再踩一遍。所以把规则写成测试，而不是写成注释。

import { describe, expect, test } from 'bun:test';

const CSS = await Bun.file(new URL('../../app.css', import.meta.url)).text();

/** 从一个 `:root` 块里抽出所有 `--x: #hex;`。 */
function tokens(selector: string): Record<string, string> {
  const start = CSS.indexOf(selector);
  if (start < 0) throw new Error(`app.css 里找不到 ${selector}`);
  const open = CSS.indexOf('{', start);
  const close = CSS.indexOf('\n}', open);
  const block = CSS.slice(open, close);
  const out: Record<string, string> = {};
  for (const [, name, hex] of block.matchAll(/(--[\w-]+):\s*(#[0-9a-fA-F]{6})\s*;/g)) {
    out[name] = hex.toLowerCase();
  }
  return out;
}

const DARK = tokens(':root {');
const LIGHT = { ...DARK, ...tokens(":root[data-theme='light'] {") };

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
  // StatusPill 把这五个当文字色用
  '--st-running',
  '--st-succeeded',
  '--st-failed',
  '--st-timeout',
  '--st-resource'
];

/** 只当圆点/描边用的 token：AA 非文字 3:1。 */
const AS_GRAPHIC = ['--st-queued', '--st-cancelled', '--st-skipped', '--accent'];

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

test('浅色主题必须自己定义全部状态色，不能继承暗色的', () => {
  const own = tokens(":root[data-theme='light'] {");
  // 继承暗色的 hex 正是最初那个 bug：同一个绿在白底上只有 2.3:1。
  for (const token of ['--st-running', '--st-succeeded', '--st-failed', '--st-timeout']) {
    expect(own[token]).toBeDefined();
    expect(own[token]).not.toBe(DARK[token]);
  }
});
