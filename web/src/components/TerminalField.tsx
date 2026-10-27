import { useEffect, useRef } from 'react';
import { Renderer, Program, Mesh, Color, Triangle } from 'ogl';

/**
 * The hero's background: a dim field of terminal glyphs.
 *
 * From React Bits' FaultyTerminal (MIT, reactbits.dev), on `ogl` (0.4 MB, no
 * deps). Changed:
 * - Turned way down: no flicker or curvature, little glitch, a third of the
 *   brightness, tinted with the accent. Not the green hacker CRT.
 * - Reports whether WebGL came up (`onLive`). Offscreen and in CI there's no
 *   WebGL, so the CSS grid stays until this says it's live.
 * - Pauses when hidden, never starts with reduced motion. Full-screen
 *   shaders aren't free on an integrated GPU.
 * - Dropped light mode, chromatic aberration and the per-prop tuning.
 */

const VERTEX = `
attribute vec2 position;
attribute vec2 uv;
varying vec2 vUv;
void main() {
  vUv = uv;
  gl_Position = vec4(position, 0.0, 1.0);
}
`;

const FRAGMENT = `
precision mediump float;

varying vec2 vUv;

uniform float iTime;
uniform vec3  iResolution;
uniform float uScale;
uniform vec2  uGridMul;
uniform float uDigitSize;
uniform float uScanline;
uniform float uGlitch;
uniform float uNoiseAmp;
uniform float uDither;
uniform vec3  uTint;
uniform vec2  uMouse;
uniform float uMouseStrength;
uniform float uLoad;
uniform float uBrightness;

float time;

float hash21(vec2 p){
  p = fract(p * 234.56);
  p += dot(p, p + 34.56);
  return fract(p.x * p.y);
}

float noise(vec2 p){
  return sin(p.x * 10.0) * sin(p.y * (3.0 + sin(time * 0.090909))) + 0.2;
}

mat2 rotate(float angle){
  float c = cos(angle);
  float s = sin(angle);
  return mat2(c, -s, s, c);
}

float fbm(vec2 p){
  p *= 1.1;
  float f = 0.0;
  float amp = 0.5 * uNoiseAmp;
  f += amp * noise(p);
  p = rotate(time * 0.02) * p * 2.0;
  amp *= 0.454545;
  f += amp * noise(p);
  p = rotate(time * 0.02) * p * 2.0;
  amp *= 0.454545;
  f += amp * noise(p);
  return f;
}

float pattern(vec2 p){
  vec2 q = vec2(fbm(p + vec2(1.0)), fbm(rotate(0.1 * time) * p + vec2(1.0)));
  vec2 r = vec2(fbm(rotate(0.1) * q), fbm(q));
  return fbm(p + r);
}

float digit(vec2 p){
  vec2 grid = uGridMul * 15.0;
  vec2 s = floor(p * grid) / grid;
  p = p * grid;
  float intensity = pattern(s * 0.1) * 1.3 - 0.03;

  // Glyphs near the pointer brighten.
  vec2 mouseWorld = uMouse * uScale;
  float toMouse = distance(s, mouseWorld);
  intensity += exp(-toMouse * 8.0) * uMouseStrength * 10.0;

  // Random per-cell delay so the field assembles instead of popping in.
  float cellDelay = fract(sin(dot(s, vec2(12.9898, 78.233))) * 43758.5453) * 0.8;
  intensity *= smoothstep(0.0, 1.0, clamp((uLoad - cellDelay) / 0.2, 0.0, 1.0));

  p = fract(p) * uDigitSize;
  float px5 = p.x * 5.0;
  float py5 = (1.0 - p.y) * 5.0;
  float x = fract(px5);
  float y = fract(py5);
  float i = floor(py5) - 2.0;
  float j = floor(px5) - 2.0;
  float f = (i * i + j * j) * 0.0625;

  float isOn = step(0.1, intensity - f);
  float lit = isOn * (0.2 + y * 0.8) * (0.75 + x * 0.25);
  return step(0.0, p.x) * step(p.x, 1.0) * step(0.0, p.y) * step(p.y, 1.0) * lit;
}

float displace(vec2 look){
  float y = look.y - mod(iTime * 0.25, 1.0);
  float window = 1.0 / (1.0 + 50.0 * y * y);
  return sin(look.y * 20.0 + iTime) * 0.0125 * (1.0 + cos(iTime * 60.0)) * window;
}

void main() {
  time = iTime * 0.333333;
  vec2 p = vUv * uScale;

  float bar = (step(mod(p.y + time * 20.0, 1.0), 0.2) * 0.4 + 1.0) * uScanline;
  p.x += displace(p) * uGlitch;

  float middle = digit(p);

  // Glow from 4 neighbours, not the original 3x3: nine fbm calls per pixel
  // spun the fan up on integrated GPUs. Looks the same at this brightness.
  const float off = 0.002;
  float sum = digit(p + vec2(-off, 0.0)) + digit(p + vec2(off, 0.0))
            + digit(p + vec2(0.0, -off)) + digit(p + vec2(0.0, off));

  vec3 col = (vec3(0.9) * middle + sum * 0.18 * bar) * uTint * uBrightness;

  if (uDither > 0.0) {
    col += (hash21(gl_FragCoord.xy) - 0.5) * (uDither * 0.003922);
  }

  // Alpha from luminance so the violet wash and cubes behind still show.
  float alpha = clamp(max(max(col.r, col.g), col.b) * 2.6, 0.0, 1.0);
  gl_FragColor = vec4(col, alpha);
}
`;

function rgb(hex: string): [number, number, number] {
  const value = parseInt(hex.replace('#', '').slice(0, 6), 16);
  return [((value >> 16) & 255) / 255, ((value >> 8) & 255) / 255, (value & 255) / 255];
}

export function TerminalField({ tint, onLive }: {
  /** Hex colour; the hero passes the accent token. */
  tint: string;
  /** true once WebGL is up; false if it never came or got lost. */
  onLive?: (live: boolean) => void;
}) {
  const host = useRef<HTMLDivElement>(null);
  const report = useRef(onLive);
  report.current = onLive;

  useEffect(() => {
    const element = host.current;
    if (!element) return;
    if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
      // Must say false, not stay quiet: the hero only shows its fallback
      // grid once told, so silence left reduced-motion users with a blank.
      report.current?.(false);
      return;
    }

    let renderer: Renderer;
    try {
      // dpr 1 on purpose: the glyph grid gains nothing from HiDPI, it just costs double.
      renderer = new Renderer({ dpr: 1, alpha: true });
    } catch {
      // No WebGL (offscreen, blocklisted driver...). The CSS grid stays.
      report.current?.(false);
      return;
    }

    const gl = renderer.gl;
    gl.clearColor(0, 0, 0, 0);
    const [r, g, b] = rgb(tint);

    const program = new Program(gl, {
      vertex: VERTEX,
      fragment: FRAGMENT,
      uniforms: {
        iTime: { value: 0 },
        iResolution: { value: new Color(1, 1, 1) },
        uScale: { value: 1.6 },
        uGridMul: { value: new Float32Array([2, 1]) },
        uDigitSize: { value: 1.3 },
        uScanline: { value: 0.26 },
        uGlitch: { value: 0.15 },
        uNoiseAmp: { value: 1 },
        uDither: { value: 0.4 },
        uTint: { value: new Color(r, g, b) },
        uMouse: { value: new Float32Array([0.5, 0.5]) },
        uMouseStrength: { value: 0.12 },
        uLoad: { value: 0 },
        uBrightness: { value: 0.28 },
      },
    });
    const mesh = new Mesh(gl, { geometry: new Triangle(gl), program });

    const size = () => {
      renderer.setSize(element.clientWidth || 1, element.clientHeight || 1);
      program.uniforms.iResolution.value = new Color(
        gl.canvas.width, gl.canvas.height, gl.canvas.width / gl.canvas.height,
      );
    };
    const observer = new ResizeObserver(size);
    observer.observe(element);
    size();
    element.appendChild(gl.canvas);
    report.current?.(true);

    const pointer = { x: 0.5, y: 0.5 };
    const eased = { x: 0.5, y: 0.5 };
    const follow = (event: PointerEvent) => {
      const box = element.getBoundingClientRect();
      pointer.x = (event.clientX - box.left) / box.width;
      pointer.y = 1 - (event.clientY - box.top) / box.height;
    };
    element.addEventListener('pointermove', follow, { passive: true });

    const lost = () => report.current?.(false);
    gl.canvas.addEventListener('webglcontextlost', lost);

    const offset = Math.random() * 100;
    let started = 0;
    let frame = requestAnimationFrame(function draw(now: number) {
      frame = requestAnimationFrame(draw);
      // Minimised still burns GPU; skip.
      if (document.hidden) return;
      if (!started) started = now;
      program.uniforms.iTime.value = (now * 0.001 + offset) * 0.4;
      // 1800ms to finish drawing in. Paired with the 1.4s fade of
      // .bb-hero::before in global.css: change one, change both, or the
      // background jumps.
      program.uniforms.uLoad.value = Math.min((now - started) / 1800, 1);
      eased.x += (pointer.x - eased.x) * 0.08;
      eased.y += (pointer.y - eased.y) * 0.08;
      const at = program.uniforms.uMouse.value as Float32Array;
      at[0] = eased.x;
      at[1] = eased.y;
      renderer.render({ scene: mesh });
    });

    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
      element.removeEventListener('pointermove', follow);
      gl.canvas.removeEventListener('webglcontextlost', lost);
      if (gl.canvas.parentElement === element) element.removeChild(gl.canvas);
      gl.getExtension('WEBGL_lose_context')?.loseContext();
      report.current?.(false);
    };
  }, [tint]);

  return <div className="bb-hero-field" ref={host} aria-hidden="true" />;
}
