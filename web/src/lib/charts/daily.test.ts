import { describe, expect, test } from 'bun:test';
import type { DailyBucket } from '$api/types/DailyBucket';
import { dayLabel, niceMax, segmentsOf, successRate, ticksFor, topRoundedBar } from './daily';

const bucket = (runs: number, succeeded: number, failed: number, spend = '0'): DailyBucket => ({
  date: '2026-09-23',
  runs,
  succeeded,
  failed,
  spend_usd: spend
});

describe('刻度', () => {
  test.each([
    [0, 1],
    [1, 1],
    [3, 5],
    [7, 10],
    [12, 20],
    [48, 50],
    [51, 100],
    [230, 500]
  ])('%d 往上取整到 %d', (value, nice) => {
    expect(niceMax(value)).toBe(nice);
  });

  test('中间刻度是整数才画，免得出现 2.5 次', () => {
    expect(ticksFor(10)).toEqual([0, 5, 10]);
    expect(ticksFor(5)).toEqual([0, 5]);
    expect(ticksFor(1)).toEqual([0, 1]);
  });
});

describe('每天的结果', () => {
  test('其它 = 总数减去成功和失败（取消、还在跑的）', () => {
    expect(segmentsOf(bucket(10, 6, 3))).toEqual([
      { key: 'failed', value: 3 },
      { key: 'succeeded', value: 6 },
      { key: 'other', value: 1 }
    ]);
  });

  test('成功率只看有结论的；一次都没结束就是没有，不是 0%', () => {
    expect(successRate(bucket(10, 6, 2))).toBe(0.75);
    expect(successRate(bucket(3, 0, 0))).toBeNull();
    expect(successRate(bucket(0, 0, 0))).toBeNull();
  });

  test('日期标签带星期，按那一天本身算，不受浏览器时区影响', () => {
    expect(dayLabel('2026-09-24')).toEqual({ short: '09-24', weekday: '周四' });
    expect(dayLabel('2026-09-27')).toEqual({ short: '09-27', weekday: '周日' });
  });
});

describe('柱子的形状', () => {
  test('只圆上面两个角，底边贴着基线是方的', () => {
    expect(topRoundedBar(10, 20, 24, 50, 4)).toBe(
      'M10,70V24Q10,20 14,20H30Q34,20 34,24V70Z'
    );
  });

  test('比圆角还矮的柱子，圆角跟着缩，不画出界', () => {
    expect(topRoundedBar(0, 0, 10, 2, 4)).toBe('M0,2V2Q0,0 2,0H8Q10,0 10,2V2Z');
  });
});
