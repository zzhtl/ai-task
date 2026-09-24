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
  'arrow-up': '<path d="M12 19V5M5 12l7-7 7 7"/>',
  'arrow-down': '<path d="M12 5v14M5 12l7 7 7-7"/>',

  // —— 主题
  sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>',
  moon: '<path d="M21 12.8A9 9 0 1111.2 3a7 7 0 009.8 9.8z"/>',
  monitor: '<path d="M4 5h16v11H4zM9 20h6M12 16v4"/>',

  // —— 状态。每种状态除了颜色还有自己的形状：色弱、黑白打印、投影仪上都分得清
  check: '<path d="M5 12.5l4.5 4.5L19 7.5"/>',
  clock: '<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5V12l3 2"/>',
  dollar: '<path d="M12 3v18M16 7.5c-.8-1.1-2.2-1.8-4-1.8-2.3 0-4 1.2-4 3 0 4 8 2 8 6.2 0 1.8-1.8 3.1-4.2 3.1-1.9 0-3.5-.7-4.3-2"/>',
  cpu: '<rect x="7" y="7" width="10" height="10" rx="1.5"/><path d="M10 3.5v3M14 3.5v3M10 17.5v3M14 17.5v3M3.5 10h3M3.5 14h3M17.5 10h3M17.5 14h3"/>',
  pause: '<circle cx="12" cy="12" r="8.5"/><path d="M10 9v6M14 9v6"/>',
  ban: '<circle cx="12" cy="12" r="8.5"/><path d="M6 6l12 12"/>',
  'circle-dashed': '<circle cx="12" cy="12" r="8" stroke-dasharray="3.2 3.2"/>',
  circle: '<circle cx="12" cy="12" r="7"/>',
  loader: '<path d="M12 3.5a8.5 8.5 0 108.5 8.5"/>',
  skip: '<path d="M5 6l6 6-6 6M13 6l6 6-6 6"/>',

  // —— 杂项
  help: '<circle cx="12" cy="12" r="9"/><path d="M9.6 9.3a2.5 2.5 0 014.8 1c0 1.7-2.4 2.1-2.4 3.7M12 17.2h.01"/>',
  copy: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V6a2 2 0 00-2-2H6a2 2 0 00-2 2v8a2 2 0 002 2h2"/>',
  sidebar: '<rect x="3.5" y="4.5" width="17" height="15" rx="2"/><path d="M9.5 4.5v15"/>',
  more: '<circle cx="5.5" cy="12" r="1"/><circle cx="12" cy="12" r="1"/><circle cx="18.5" cy="12" r="1"/>'
} as const;

export type IconName = keyof typeof ICONS;
