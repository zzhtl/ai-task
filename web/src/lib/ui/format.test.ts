import { describe, expect, test } from 'bun:test';
import { clock, duration, money, stamp, toMicros, moneyMicros } from './format';

describe('绝对时间按东八区显示', () => {
  // 这个测试在任何时区的机器上都必须过：显示时区是定死的，不跟机器走
  test('UTC 的 ISO 串显示成 +8 的墙钟时间', () => {
    expect(stamp('2026-09-09T10:55:21Z')).toBe('2026-09-09 18:55:21');
    expect(clock('2026-09-09T10:55:21Z')).toBe('18:55:21');
  });

  test('跨日：UTC 晚上 20 点是东八区次日凌晨 4 点', () => {
    expect(stamp('2026-09-09T20:00:00Z')).toBe('2026-09-10 04:00:00');
  });

  test('午夜是 00 不是 24', () => {
    expect(clock('2026-09-09T16:00:00Z')).toBe('00:00:00');
  });

  test('带偏移量的输入先换算再显示', () => {
    expect(stamp('2026-09-09T04:20:15-07:00')).toBe('2026-09-09 19:20:15');
  });

  test('空值和坏输入给横线，不抛', () => {
    expect(stamp(null)).toBe('—');
    expect(stamp('not a date')).toBe('—');
    expect(clock(undefined)).toBe('—');
  });
});

describe('其它格式化', () => {
  test('耗时', () => {
    expect(duration('2026-09-09T00:00:00Z', '2026-09-09T00:00:07.400Z')).toBe('7.4s');
    expect(duration('2026-09-09T00:00:00Z', '2026-09-09T01:05:00Z')).toBe('1h 5m');
    expect(duration(null)).toBe('—');
  });
  test('金额', () => {
    expect(money('0.012345')).toBe('$0.01');
    expect(money('0.001234')).toBe('$0.0012');
    expect(money('0')).toBe('$0');
  });
});

describe('金额换成整数微美元', () => {
  test.each([
    ['0.012345', 12345],
    ['1', 1_000_000],
    ['0.5', 500_000],
    ['12.3456789', 12_345_678],
    ['-0.000001', -1],
    ['', 0],
    ['abc', 0]
  ])('%s → %d', (text, micros) => {
    expect(toMicros(text)).toBe(micros);
  });

  test('加一百次 0.01 还是 1 美元（浮点加会漂到 1.0000000000000007）', () => {
    let sum = 0;
    for (let i = 0; i < 100; i++) sum += toMicros('0.010000');
    expect(sum).toBe(1_000_000);
    expect(moneyMicros(sum)).toBe('$1.00');
  });
});
