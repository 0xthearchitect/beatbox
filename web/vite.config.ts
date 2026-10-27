import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';

/**
 * CSP: the page loads almost nothing. Guest output (command answers, file
 * names) ends up on the page, so "never fetch" has to be enforced, not promised.
 *
 *   script-src   the bundle only.
 *   style-src    'unsafe-inline' is needed for React style={{}} props. Styling, not code.
 *   connect-src  Tauri's IPC (`ipc:` on Linux, `http://ipc.localhost` on
 *                Windows; both in-process) and QEMU's VNC WebSocket on
 *                127.0.0.1. With 'none' Tauri fell back to postMessage and
 *                sent the same requests anyway, so name the real transport.
 *
 * Injected at build time only.
 */
const POLICY = [
  "default-src 'none'",
  "script-src 'self'",
  "style-src 'self' 'unsafe-inline'",
  "img-src 'self' data:",
  "font-src 'self'",
  // Machine displays: QEMU VNC on loopback, per-boot password.
  "connect-src ipc: http://ipc.localhost ws://127.0.0.1:*",
  "form-action 'none'",
  // No frame-ancestors: ignored in a <meta>, and Chromium warns about it on every load.
  "base-uri 'none'",
].join('; ');

const contentSecurityPolicy = {
  name: 'beatbox-csp',
  apply: 'build' as const,
  transformIndexHtml(html: string) {
    return html.replace(
      '<meta charset="UTF-8" />',
      `<meta charset="UTF-8" />\n    <meta http-equiv="Content-Security-Policy" content="${POLICY}" />`,
    );
  },
};

// Embedded in the window binary, so relative URLs. No content hashes: dist/
// is committed and hashes would make every rebuild a giant diff, with no
// browser cache to bust anyway.
export default defineConfig({
  base: './',
  plugins: [react(), tailwindcss(), contentSecurityPolicy],
  // `@/` = src/, what the shadcn components import through.
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'chrome110',
    sourcemap: false,
    rollupOptions: {
      output: {
        entryFileNames: 'assets/[name].js',
        chunkFileNames: 'assets/[name].js',
        assetFileNames: 'assets/[name][extname]',
      },
    },
  },
});
