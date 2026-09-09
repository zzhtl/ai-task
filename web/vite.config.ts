import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [sveltekit()],
  server: {
    port: 5173,
    // 开发期前后端同源：/api 与 /events 直接代理到后端，不依赖 CORS。
    // 生产是同一个二进制 serve，本来就同源。
    proxy: {
      '/api': { target: 'http://127.0.0.1:8930', changeOrigin: false },
      // SSE 必须关掉代理层的缓冲，否则事件会攒够一块才吐出来
      '/internal': { target: 'http://127.0.0.1:8930', changeOrigin: false }
    }
  }
});
