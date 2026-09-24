import { expect, test } from 'bun:test';
import { fromSpec, toSpec } from './compose';
import { TEMPLATES } from './templates';

// 选了模板什么都不改就保存，存进去的必须就是模板本身：
// 编辑器要是在往返里改了什么，那是在替用户做决定
test.each(TEMPLATES.filter((t) => t.spec).map((t) => [t.name, t] as const))(
  '%s 能原样进出步骤编辑器',
  (_, template) => {
    const comp = fromSpec(template.spec!);
    expect(comp).not.toBeNull();
    expect(toSpec(comp!)).toEqual(template.spec!);
  }
);
