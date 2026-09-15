// 图标表。
//
// 之前图标是一串 `<path d="…">` 直接内联在用到的地方，于是垃圾桶被抄了 7 遍、
// 铅笔 5 遍。抄出去的那几份没人会同步——改描边粗细得翻十二个文件，
// 而拼错一个 d 字符串不会报错，只会画出一团乱线。
//
// 统一 24×24 视框、`fill: none` + `stroke: currentColor`（描边宽度在 app.css 里）。
// 值是 SVG 的原始内容，所以少数图标可以带 <circle>，不必都是单条 path。

export const ICONS = {
  // —— 导航
  activity: '<path d="M3 12h4l3-8 4 16 3-8h4"/>',
  list: '<path d="M4 6h16M4 12h16M4 18h10"/>',
  play: '<path d="M5 3l14 9-14 9V3z"/>',
  shield: '<path d="M12 3l8 4v6c0 4-3.5 7-8 8-4.5-1-8-4-8-8V7z"/>',
  'shield-check': '<path d="M9 12l2 2 4-4M12 3l8 4v6c0 4-3.5 7-8 8-4.5-1-8-4-8-8V7z"/>',
  server: '<path d="M4 6h16v5H4zM4 14h16v5H4zM8 8.5h.01M8 16.5h.01"/>',
  users: '<path d="M4 20c0-3 3.6-5 8-5s8 2 8 5M12 11a4 4 0 100-8 4 4 0 000 8"/>',
  file: '<path d="M8 4h8l4 4v12H4V4h4zM8 12h8M8 16h5"/>',

  // —— 动作
  trash: '<path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3"/>',
  pencil: '<path d="M4 20h4l10-10-4-4L4 16v4zM13 7l4 4"/>',
  close: '<path d="M6 6l12 12M18 6L6 18"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  search: '<circle cx="11" cy="11" r="7"/><path d="M20 20l-3.5-3.5"/>',
  logout: '<path d="M9 21H5a2 2 0 01-2-2V5a2 2 0 012-2h4M16 17l5-5-5-5M21 12H9"/>',

  // —— 方向
  'chevron-down': '<path d="M6 9l6 6 6-6"/>',
  'chevron-right': '<path d="M9 6l6 6-6 6"/>',
  'chevron-left': '<path d="M15 6l-6 6 6 6"/>',
  'arrow-up': '<path d="M12 19V5M5 12l7-7 7 7"/>',
  'arrow-down': '<path d="M12 5v14M5 12l7 7 7-7"/>',

  // —— 主题
  sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>',
  moon: '<path d="M21 12.8A9 9 0 1111.2 3a7 7 0 009.8 9.8z"/>'
} as const;

export type IconName = keyof typeof ICONS;
