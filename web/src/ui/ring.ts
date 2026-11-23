// The hero ring: a particle model of Saturn's A ring around the Keeler Gap, in raw WebGL2.
// Particles drift at Keplerian speed; the moon Daphnis orbits in the gap and raises waves on its edges,
// trailing it on the outer edge and leading it on the inner edge.

export type DealerLight = "waiting" | "answered" | "silent";
export type RingInput = { dealers: DealerLight[] | null; proving: boolean; settled: number };
export type RingLayout = { cx: number; cy: number; radius: number };

const GAP_R = 0.95;
const GAP_HALF = 0.016;
const R_IN = 0.5;
const R_OUT = 1.2;
const DIST = 3.4;
const OMEGA = 0.075; // rad/s at r = 1
const TILT = 0.4;
const ROLL = -0.22;
const MOON0 = 2.15; // moon angle at t = 0: on the near side, lower right

const gauss = (x: number, m: number, s: number) => Math.exp(-((x - m) ** 2) / (2 * s * s));
const smooth = (a: number, b: number, x: number) => {
  const t = Math.min(1, Math.max(0, (x - a) / (b - a)));
  return t * t * (3 - 2 * t);
};

/** Optical depth of the ring at radius r, roughly 0..1.3: bands, ringlets, one clear gap. */
function profile(r: number): number {
  if (Math.abs(r - GAP_R) < GAP_HALF) return 0;
  const env = smooth(R_IN, R_IN + 0.07, r) * (1 - smooth(R_OUT - 0.09, R_OUT, r));
  let b = 0.28 + 0.75 * gauss(r, 0.8, 0.075) + 0.4 * gauss(r, 1.05, 0.04) + 0.22 * gauss(r, 0.6, 0.025);
  b *= 0.78 + 0.22 * Math.sin(r * 210) * Math.sin(r * 53 + 1.3);
  b *= 1 - 0.45 * gauss(r, 0.69, 0.006);
  // Edges of the gap are slightly brighter, as real gap edges are.
  b += 0.25 * gauss(Math.abs(r - GAP_R) - GAP_HALF, 0, 0.006);
  return env * b;
}

const VS = `#version 300 es
precision highp float;
layout(location = 0) in vec4 aA; // radius, start angle, height, seed
layout(location = 1) in vec4 aB; // density, size, streak, sparkle
uniform float uTime, uFocal, uDpr, uMoon, uWave, uSettle, uSettleOn, uHaze;
uniform vec2 uRes, uCenter;
uniform mat3 uRot;
out vec3 vCol;
out vec2 vDir;
out float vStretch, vInt;
const float PI = 3.14159265;
vec2 proj(vec3 v) { return uCenter + v.xy * (uFocal / (${DIST.toFixed(2)} + v.z)); }
void main() {
  float r = aA.x;
  float seed = aA.w;
  float w = ${OMEGA.toFixed(4)} * pow(r, -1.5);
  float ang = aA.y + w * uTime;
  // Daphnis: relative angle to the moon and the damped edge wave downstream of it.
  float d = mod(ang - uMoon + PI, 2.0 * PI) - PI;
  float s = r - ${GAP_R.toFixed(3)};
  float edge = max(abs(s) - ${GAP_HALF.toFixed(3)}, 0.0);
  float down = s > 0.0 ? step(d, 0.0) : step(0.0, d);
  float x = abs(d);
  float env = exp(-x * 1.4) * smoothstep(0.0, 0.1, x) * exp(-edge / 0.03) * down;
  float wave = sin(x * 34.0 - 0.6) * env;
  r += uWave * wave;
  float h = aA.z + uWave * 0.7 * wave * (1.0 - step(0.0, -s) * 2.0);
  vec3 p = vec3(r * cos(ang), h, r * sin(ang));
  vec3 v = uRot * p;
  vec2 px = proj(v);
  vec3 t = uRot * vec3(-sin(ang), 0.0, cos(ang));
  vec2 dir = proj(v + t * 0.01) - px;
  vDir = length(dir) > 0.0 ? normalize(dir) : vec2(1.0, 0.0);
  float persp = ${DIST.toFixed(2)} / (${DIST.toFixed(2)} + v.z);
  float size = aB.y * uFocal * 0.00125 * persp;
  float streak = aB.z * pow(r, -1.5) * (1.0 - uHaze);
  vStretch = 1.0 + streak;
  // Light: denser bands run hotter, the near side scatters a little more, sparkles twinkle.
  float dens = aB.x;
  vec3 gold = vec3(0.914, 0.722, 0.447);
  vec3 ember = vec3(1.0, 0.416, 0.239);
  vec3 ice = vec3(0.957, 0.922, 0.839);
  vec3 col = mix(gold, ember, smoothstep(0.62, 1.0, dens));
  float inten = (0.1 + 0.2 * dens) * (1.0 - 0.35 * v.z) / sqrt(vStretch);
  inten *= 1.0 + 1.6 * abs(wave) * min(uWave * 60.0, 1.0);
  if (aB.w > 0.5) {
    col = ice;
    inten = 0.25 + 0.75 * pow(0.5 + 0.5 * sin(uTime * (1.1 + seed * 2.7) + seed * 61.0), 8.0);
  }
  // Settled: one bright front runs once round the ring, with a fading tail.
  float f = mod(ang - uSettle + PI, 2.0 * PI) - PI;
  float front = f < 0.0 ? exp(f * 2.2) : exp(-f * 30.0);
  inten *= 1.0 + uSettleOn * 5.0 * front;
  col = mix(col, ice, uSettleOn * 0.6 * front);
  if (uHaze > 0.5) {
    size *= 9.0;
    inten *= 0.022;
    col = mix(col, ember, 0.4);
  }
  vCol = col;
  vInt = inten;
  gl_PointSize = max(size * vStretch, 1.0);
  gl_Position = vec4(px.x / uRes.x * 2.0 - 1.0, 1.0 - px.y / uRes.y * 2.0, 0.0, 1.0);
}`;

const FS = `#version 300 es
precision mediump float;
in vec3 vCol;
in vec2 vDir;
in float vStretch, vInt;
out vec4 o;
void main() {
  vec2 c = gl_PointCoord * 2.0 - 1.0;
  float a = dot(c, vDir);
  float b = dot(c, vec2(-vDir.y, vDir.x)) * vStretch;
  float d2 = a * a + b * b;
  if (d2 > 1.0) discard;
  // A tail that fades behind the particle reads as motion, not as a dash.
  float tail = vStretch > 1.01 ? smoothstep(-1.0, 0.7, a) : 1.0;
  o = vec4(vCol * vInt * exp(-d2 * 4.0) * tail, 1.0);
}`;

// Moon and dealer lights: a handful of screen-space glows.
const SVS = `#version 300 es
layout(location = 0) in vec2 aPos;
layout(location = 1) in vec4 aCol; // rgb, size in device px in w
uniform vec2 uRes;
out vec3 vCol;
void main() {
  vCol = aCol.rgb;
  gl_PointSize = aCol.w;
  gl_Position = vec4(aPos.x / uRes.x * 2.0 - 1.0, 1.0 - aPos.y / uRes.y * 2.0, 0.0, 1.0);
}`;
const SFS = `#version 300 es
precision mediump float;
in vec3 vCol;
out vec4 o;
void main() {
  float d = length(gl_PointCoord * 2.0 - 1.0);
  if (d > 1.0) discard;
  o = vec4(vCol * (exp(-d * d * 18.0) * 1.4 + exp(-d * 4.5) * 0.35) * (1.0 - d), 1.0);
}`;

function compile(gl: WebGL2RenderingContext, vs: string, fs: string): WebGLProgram {
  const p = gl.createProgram()!;
  for (const [type, src] of [[gl.VERTEX_SHADER, vs], [gl.FRAGMENT_SHADER, fs]] as const) {
    const s = gl.createShader(type)!;
    gl.shaderSource(s, src);
    gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s) ?? "shader");
    gl.attachShader(p, s);
  }
  gl.linkProgram(p);
  if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(p) ?? "link");
  return p;
}

function particles(n: number): Float32Array {
  const data = new Float32Array(n * 8);
  let i = 0;
  while (i < n) {
    const r = R_IN + Math.random() * (R_OUT - R_IN);
    const dens = profile(r);
    if (Math.random() * 1.35 > dens) continue;
    const k = i * 8;
    const sparkle = Math.random() < 0.012;
    data[k] = r;
    data[k + 1] = Math.random() * Math.PI * 2;
    data[k + 2] = (Math.random() - 0.5) * 0.006;
    data[k + 3] = Math.random();
    data[k + 4] = Math.min(dens / 1.15, 1);
    data[k + 5] = sparkle ? 2.6 : 0.9 + Math.random() ** 3 * 1.6;
    data[k + 6] = Math.random() < 0.22 ? 2 + Math.random() * 5 : 0;
    data[k + 7] = sparkle ? 1 : 0;
    i++;
  }
  return data;
}

const ROT = (() => {
  const ct = Math.cos(TILT), st = Math.sin(TILT), cr = Math.cos(ROLL), sr = Math.sin(ROLL);
  // Rz(roll) * Rx(tilt), column-major.
  return new Float32Array([cr, sr, 0, -sr * ct, cr * ct, st, sr * st, -cr * st, ct]);
})();

function rotate(p: [number, number, number]): [number, number, number] {
  const m = ROT;
  return [
    m[0]! * p[0] + m[3]! * p[1] + m[6]! * p[2],
    m[1]! * p[0] + m[4]! * p[1] + m[7]! * p[2],
    m[2]! * p[0] + m[5]! * p[1] + m[8]! * p[2],
  ];
}

const DEALER_R = 1.13;
const DEALER_ANGLES = [1.25, 1.75, 2.6, 3.1, 0.75];

export class Ring {
  private gl: WebGL2RenderingContext;
  private prog: WebGLProgram;
  private sprog: WebGLProgram;
  private vao: WebGLVertexArrayObject;
  private svao: WebGLVertexArrayObject;
  private sbuf: WebGLBuffer;
  private count: number;
  private loc: Record<string, WebGLUniformLocation | null> = {};
  private sres: WebGLUniformLocation | null;
  private layoutPx: RingLayout = { cx: 0, cy: 0, radius: 1 };
  private dpr = 1;
  private time = 40; // start mid-orbit so the first frame already shows the moon's wake
  private last = 0;
  private raf = 0;
  private running = false;
  private visible = true;
  private onScreen = true;
  private input: RingInput = { dealers: null, proving: false, settled: 0 };
  private wave = 0.006;
  private settleT = -1;
  private settledSeen = 0;
  private lights: { on: number; flash: number; state: DealerLight }[] = [];

  constructor(
    private canvas: HTMLCanvasElement,
    private still: boolean,
  ) {
    const gl = canvas.getContext("webgl2", { alpha: false, antialias: false, powerPreference: "high-performance" });
    if (!gl) throw new Error("no webgl2");
    this.gl = gl;
    this.prog = compile(gl, VS, FS);
    this.sprog = compile(gl, SVS, SFS);
    for (const u of ["uTime", "uFocal", "uDpr", "uMoon", "uWave", "uSettle", "uSettleOn", "uHaze", "uRes", "uCenter", "uRot"])
      this.loc[u] = gl.getUniformLocation(this.prog, u);
    this.sres = gl.getUniformLocation(this.sprog, "uRes");

    const small = Math.min(innerWidth, innerHeight) < 700 || (navigator.hardwareConcurrency ?? 8) <= 4;
    this.count = small ? 38_000 : 72_000;
    this.vao = gl.createVertexArray()!;
    gl.bindVertexArray(this.vao);
    const buf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buf);
    gl.bufferData(gl.ARRAY_BUFFER, particles(this.count), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 4, gl.FLOAT, false, 32, 0);
    gl.enableVertexAttribArray(1);
    gl.vertexAttribPointer(1, 4, gl.FLOAT, false, 32, 16);

    this.svao = gl.createVertexArray()!;
    gl.bindVertexArray(this.svao);
    this.sbuf = gl.createBuffer()!;
    gl.bindBuffer(gl.ARRAY_BUFFER, this.sbuf);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 24, 0);
    gl.enableVertexAttribArray(1);
    gl.vertexAttribPointer(1, 4, gl.FLOAT, false, 24, 8);
    gl.bindVertexArray(null);

    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE);
    gl.clearColor(0, 0, 0, 1);

    document.addEventListener("visibilitychange", this.onVisibility);
    this.io = new IntersectionObserver(([e]) => {
      this.onScreen = !!e?.isIntersecting;
      this.sync();
    });
    this.io.observe(canvas);
  }
  private io: IntersectionObserver;

  /** Canvas size in CSS px and where the ring sits within it. */
  resize(width: number, height: number, layout: RingLayout) {
    this.dpr = Math.min(devicePixelRatio || 1, 2);
    this.canvas.width = Math.round(width * this.dpr);
    this.canvas.height = Math.round(height * this.dpr);
    this.layoutPx = layout;
    this.gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    this.frame(0);
  }

  set(input: RingInput) {
    this.input = input;
    const n = input.dealers?.length ?? 0;
    while (this.lights.length < n) this.lights.push({ on: 0, flash: 0, state: "waiting" });
    input.dealers?.forEach((s, i) => {
      const l = this.lights[i]!;
      if (s === "answered" && l.state !== "answered") l.flash = 1;
      l.state = s;
    });
    if (input.settled !== this.settledSeen) {
      this.settledSeen = input.settled;
      if (input.settled > 0 && !this.still) this.settleT = 0;
    }
    if (this.still) this.frame(0);
    this.sync();
  }

  destroy() {
    this.running = false;
    cancelAnimationFrame(this.raf);
    document.removeEventListener("visibilitychange", this.onVisibility);
    this.io.disconnect();
    this.gl.getExtension("WEBGL_lose_context")?.loseContext();
  }

  private onVisibility = () => {
    this.visible = document.visibilityState === "visible";
    this.sync();
  };

  private sync() {
    const want = !this.still && this.visible && this.onScreen;
    if (want && !this.running) {
      this.running = true;
      this.last = performance.now();
      this.raf = requestAnimationFrame(this.loop);
    } else if (!want && this.running) {
      this.running = false;
      cancelAnimationFrame(this.raf);
    }
  }

  private loop = (now: number) => {
    if (!this.running) return;
    const dt = Math.min((now - this.last) / 1000, 0.1);
    this.last = now;
    this.frame(dt);
    this.raf = requestAnimationFrame(this.loop);
  };

  private frame(dt: number) {
    const gl = this.gl;
    this.time += dt;
    const ease = (k: number) => 1 - Math.exp(-dt * k);
    const still = this.still;
    // Smooth inputs; the still frame jumps straight to the target.
    const waveTarget = this.input.proving ? 0.03 : 0.006;
    this.wave = still ? waveTarget : this.wave + (waveTarget - this.wave) * ease(2.2);
    let settleOn = 0;
    let settleAng = 0;
    if (this.settleT >= 0) {
      this.settleT += dt / 3.2;
      if (this.settleT >= 1) this.settleT = -1;
      else {
        settleOn = Math.sin(Math.PI * Math.min(this.settleT * 4, 1)) * 0.3 + Math.min(this.settleT * 6, 1) * (1 - this.settleT) * 0.9;
        settleAng = 1.0 + this.settleT * Math.PI * 2.2;
      }
    }

    const W = this.canvas.width, H = this.canvas.height, d = this.dpr;
    const { cx, cy, radius } = this.layoutPx;
    const focal = ((radius * d) / R_OUT) * DIST;
    const moon = MOON0 + OMEGA * GAP_R ** -1.5 * this.time;

    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.useProgram(this.prog);
    gl.uniform1f(this.loc.uTime!, this.time);
    gl.uniform1f(this.loc.uFocal!, focal);
    gl.uniform1f(this.loc.uDpr!, d);
    gl.uniform1f(this.loc.uMoon!, moon);
    gl.uniform1f(this.loc.uWave!, this.wave);
    gl.uniform1f(this.loc.uSettle!, settleAng);
    gl.uniform1f(this.loc.uSettleOn!, settleOn);
    gl.uniform2f(this.loc.uRes!, W, H);
    gl.uniform2f(this.loc.uCenter!, cx * d, cy * d);
    gl.uniformMatrix3fv(this.loc.uRot!, false, ROT);
    gl.bindVertexArray(this.vao);
    gl.uniform1f(this.loc.uHaze!, 1);
    gl.drawArrays(gl.POINTS, 0, Math.floor(this.count / 12));
    gl.uniform1f(this.loc.uHaze!, 0);
    gl.drawArrays(gl.POINTS, 0, this.count);

    // Moon and dealer lights, projected with the same camera.
    const project = (r: number, a: number) => {
      const v = rotate([r * Math.cos(a), 0, r * Math.sin(a)]);
      const s = focal / (DIST + v[2]);
      return [cx * d + v[0] * s, cy * d + v[1] * s, DIST / (DIST + v[2])] as const;
    };
    const sprites: number[] = [];
    const m = project(GAP_R, moon);
    const ms = (radius * d) / 140;
    sprites.push(m[0], m[1], 0.96, 0.92, 0.84, Math.max(14, 26 * ms) * m[2]);
    const pulse = 0.5 + 0.5 * Math.sin(this.time * 4.2);
    this.lights.forEach((l, i) => {
      const target = this.input.dealers && l.state !== "silent" ? 1 : 0;
      l.on = still ? target : l.on + (target - l.on) * ease(5);
      l.flash = still ? 0 : l.flash * (1 - ease(2.5));
      if (l.on < 0.01) return;
      const k = l.on * (l.state === "waiting" ? 0.45 + 0.4 * pulse : 0.7) + l.flash * 1.3;
      const a = DEALER_ANGLES[i % DEALER_ANGLES.length]! + OMEGA * DEALER_R ** -1.5 * this.time * 0.25;
      const p = project(DEALER_R, a);
      const size = Math.max(16, 30 * ms) * p[2] * (1 + l.flash * 0.8);
      sprites.push(p[0], p[1], 1.0 * k, 0.55 * k, 0.32 * k, size);
    });
    gl.useProgram(this.sprog);
    gl.uniform2f(this.sres, W, H);
    gl.bindVertexArray(this.svao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.sbuf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(sprites), gl.DYNAMIC_DRAW);
    gl.drawArrays(gl.POINTS, 0, sprites.length / 6);
    gl.bindVertexArray(null);
  }
}
