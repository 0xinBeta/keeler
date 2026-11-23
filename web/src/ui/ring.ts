// The hero ring: a particle model of Saturn's A ring around the Keeler Gap, in raw WebGL2.
// Particles drift at Keplerian speed; the moon Daphnis orbits in the gap and raises waves on its edges,
// trailing it on the outer edge and leading it on the inner edge.

export type DealerLight = "waiting" | "answered" | "silent";
export type RingInput = { dealers: DealerLight[] | null; proving: boolean; settled: number };
/** Centre and outer radius in CSS px; `lights` is the ring angle where dealer lights start, on an arc left visible. */
export type RingLayout = { cx: number; cy: number; radius: number; lights: number };

const GAP_R = 0.95;
const GAP_HALF = 0.02;
const R_IN = 0.5;
const R_OUT = 1.2;
const DIST = 3.4;
const OMEGA = 0.075; // rad/s at r = 1
const TILT = 0.4;
const ROLL = -0.22;
const MOON0 = 1.16; // so the moon crosses the near arc, below the panel, in the first seconds

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
  // While proving the wake reaches further round the ring, so the change shows wherever the moon is.
  float prove = clamp((uWave - 0.005) / 0.012, 0.0, 1.0);
  float env = exp(-x * mix(0.8, 0.3, prove)) * smoothstep(0.0, 0.08, x) * exp(-edge / 0.028) * down;
  float wave = sin(x * 22.0 - 0.6) * env;
  r += uWave * wave;
  float h = aA.z + uWave * 0.7 * wave * (1.0 - step(0.0, -s) * 2.0);
  vec3 p = vec3(r * cos(ang), h, r * sin(ang));
  vec3 v = uRot * p;
  vec2 px = proj(v);
  vec3 t = uRot * vec3(-sin(ang), 0.0, cos(ang));
  vec2 dir = proj(v + t * 0.01) - px;
  vDir = length(dir) > 0.0 ? normalize(dir) : vec2(1.0, 0.0);
  float persp = ${DIST.toFixed(2)} / (${DIST.toFixed(2)} + v.z);
  float size = aB.y * uFocal * 0.0015 * persp;
  float streak = aB.z * pow(r, -1.5) * (1.0 - uHaze);
  vStretch = 1.0 + streak;
  // Light: denser bands run hotter, the near side scatters a little more, sparkles twinkle.
  float dens = aB.x;
  // Linear light, more saturated than the palette: under the tone curve they settle on gold #E9B872 and
  // ember #FF6A3D at the intensities the ring reaches, and only the densest overlaps run white-hot.
  vec3 gold = vec3(1.0, 0.56, 0.24);
  vec3 ember = vec3(1.0, 0.27, 0.1);
  vec3 ice = vec3(1.0, 0.95, 0.86);
  vec3 col = mix(gold, ember, smoothstep(0.5, 0.95, dens));
  float inten = (0.16 + 0.5 * dens * dens) * (1.0 - 0.4 * v.z) / sqrt(vStretch);
  inten *= 1.0 + 1.6 * abs(wave) * min(uWave * 60.0, 1.0) + prove * 1.2 * exp(-edge / 0.02);
  col = mix(col, ember, prove * 0.5 * exp(-edge / 0.02));
  if (aB.w > 0.5) {
    col = ice;
    inten = 0.3 + 1.6 * pow(0.5 + 0.5 * sin(uTime * (1.1 + seed * 2.7) + seed * 61.0), 8.0);
  }
  // Settled: one bright front runs once round the ring, with a fading tail.
  float f = mod(ang - uSettle + PI, 2.0 * PI) - PI;
  float front = f < 0.0 ? exp(f * 2.2) : exp(-f * 30.0);
  inten *= 1.0 + uSettleOn * 5.0 * front;
  col = mix(col, ice, uSettleOn * 0.6 * front);
  if (uHaze > 0.5) {
    size *= 7.0;
    inten *= 0.03;
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
  o = vec4(vCol * (exp(-d * d * 70.0) * 4.0 + exp(-d * 7.0) * 0.45) * (1.0 - d), 1.0);
}`;

// Post: the particles add up in a float buffer; a blurred copy is the glow; a tone curve keeps it light, not clipped.
const QVS = `#version 300 es
out vec2 vUv;
void main() {
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  vUv = p;
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}`;
const BLUR = `#version 300 es
precision mediump float;
uniform sampler2D uTex;
uniform vec2 uStep;
uniform float uLod;
in vec2 vUv;
out vec4 o;
vec3 at(vec2 d) { return textureLod(uTex, vUv + d, uLod).rgb; }
void main() {
  vec3 c = at(vec2(0.0)) * 0.227;
  c += (at(uStep * 1.385) + at(-uStep * 1.385)) * 0.316;
  c += (at(uStep * 3.231) + at(-uStep * 3.231)) * 0.07;
  o = vec4(c, 1.0);
}`;
const COMPOSE = `#version 300 es
precision mediump float;
uniform sampler2D uScene, uGlow;
in vec2 vUv;
out vec4 o;
void main() {
  vec3 c = texture(uScene, vUv).rgb + texture(uGlow, vUv).rgb * 1.6;
  o = vec4(1.0 - exp(-c * 1.15), 1.0);
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
    data[k + 6] = Math.random() < 0.3 ? 2 + Math.random() * 8 : 0;
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

const DEALER_R = 0.88;

type Target = { tex: WebGLTexture; fb: WebGLFramebuffer; w: number; h: number };

export class Ring {
  private gl: WebGL2RenderingContext;
  private blur: WebGLProgram;
  private compose: WebGLProgram;
  private hdr: boolean;
  private targets: { scene: Target; a: Target; b: Target } | null = null;
  private prog: WebGLProgram;
  private sprog: WebGLProgram;
  private vao: WebGLVertexArrayObject;
  private svao: WebGLVertexArrayObject;
  private sbuf: WebGLBuffer;
  private count: number;
  private loc: Record<string, WebGLUniformLocation | null> = {};
  private sres: WebGLUniformLocation | null;
  private layoutPx: RingLayout = { cx: 0, cy: 0, radius: 1, lights: 0 };
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
    /** Fraction of the full particle count; the closing section's arc needs fewer. */
    scale = 1,
  ) {
    const gl = canvas.getContext("webgl2", { alpha: false, antialias: false, powerPreference: "high-performance" });
    if (!gl) throw new Error("no webgl2");
    this.gl = gl;
    this.prog = compile(gl, VS, FS);
    this.sprog = compile(gl, SVS, SFS);
    for (const u of ["uTime", "uFocal", "uDpr", "uMoon", "uWave", "uSettle", "uSettleOn", "uHaze", "uRes", "uCenter", "uRot"])
      this.loc[u] = gl.getUniformLocation(this.prog, u);
    this.sres = gl.getUniformLocation(this.sprog, "uRes");
    this.blur = compile(gl, QVS, BLUR);
    this.compose = compile(gl, QVS, COMPOSE);
    gl.useProgram(this.compose);
    gl.uniform1i(gl.getUniformLocation(this.compose, "uScene"), 0);
    gl.uniform1i(gl.getUniformLocation(this.compose, "uGlow"), 1);
    // Without float render targets the light clips sooner, but the ring still draws.
    this.hdr = !!gl.getExtension("EXT_color_buffer_float");

    const small = Math.min(innerWidth, innerHeight) < 700 || (navigator.hardwareConcurrency ?? 8) <= 4;
    this.count = Math.round((small ? 38_000 : 72_000) * scale);
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
    this.allocate();
    this.frame(0);
  }

  private allocate() {
    const gl = this.gl;
    if (this.targets) for (const t of Object.values(this.targets)) {
      gl.deleteTexture(t.tex);
      gl.deleteFramebuffer(t.fb);
    }
    const make = (w: number, h: number, mips: boolean): Target => {
      const tex = gl.createTexture()!;
      gl.bindTexture(gl.TEXTURE_2D, tex);
      const levels = mips ? Math.floor(Math.log2(Math.max(w, h))) + 1 : 1;
      gl.texStorage2D(gl.TEXTURE_2D, levels, this.hdr ? gl.RGBA16F : gl.RGBA8, w, h);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, mips ? gl.LINEAR_MIPMAP_LINEAR : gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      const fb = gl.createFramebuffer()!;
      gl.bindFramebuffer(gl.FRAMEBUFFER, fb);
      gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0);
      return { tex, fb, w, h };
    };
    const W = Math.max(this.canvas.width, 4), H = Math.max(this.canvas.height, 4);
    this.targets = { scene: make(W, H, true), a: make(W >> 2, H >> 2, false), b: make(W >> 2, H >> 2, false) };
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
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
    if (!this.targets) return;
    this.time += dt;
    const ease = (k: number) => 1 - Math.exp(-dt * k);
    const still = this.still;
    // Smooth inputs; the still frame jumps straight to the target.
    const waveTarget = this.input.proving ? 0.017 : 0.005;
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

    const t = this.targets!;
    gl.bindFramebuffer(gl.FRAMEBUFFER, t.scene.fb);
    gl.viewport(0, 0, t.scene.w, t.scene.h);
    gl.enable(gl.BLEND);
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
    sprites.push(m[0], m[1], 1.0, 0.95, 0.86, Math.max(12, 16 * ms) * m[2]);
    const pulse = 0.5 + 0.5 * Math.sin(this.time * 4.2);
    this.lights.forEach((l, i) => {
      const target = this.input.dealers && l.state !== "silent" ? 1 : 0;
      l.on = still ? target : l.on + (target - l.on) * ease(5);
      l.flash = still ? 0 : l.flash * (1 - ease(2.5));
      if (l.on < 0.01) return;
      const k = 1.6 * (l.on * (l.state === "waiting" ? 0.45 + 0.4 * pulse : 0.8) + l.flash * 1.3);
      const a = this.layoutPx.lights + i * 0.24 + OMEGA * DEALER_R ** -1.5 * this.time * 0.1;
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

    // Glow: blur a quarter-size copy twice, then add it back under a soft tone curve.
    gl.disable(gl.BLEND);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, t.scene.tex);
    gl.generateMipmap(gl.TEXTURE_2D);
    gl.useProgram(this.blur);
    const step = gl.getUniformLocation(this.blur, "uStep");
    const lod = gl.getUniformLocation(this.blur, "uLod");
    const pass = (from: WebGLTexture, to: Target, dx: number, dy: number, l: number) => {
      gl.bindFramebuffer(gl.FRAMEBUFFER, to.fb);
      gl.viewport(0, 0, to.w, to.h);
      gl.bindTexture(gl.TEXTURE_2D, from);
      gl.uniform2f(step, dx / to.w, dy / to.h);
      gl.uniform1f(lod, l);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
    };
    pass(t.scene.tex, t.a, 1, 0, 2);
    pass(t.a.tex, t.b, 0, 1, 0);
    pass(t.b.tex, t.a, 2.5, 0, 0);
    pass(t.a.tex, t.b, 0, 2.5, 0);
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.viewport(0, 0, W, H);
    gl.useProgram(this.compose);
    gl.bindTexture(gl.TEXTURE_2D, t.scene.tex);
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, t.b.tex);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    gl.activeTexture(gl.TEXTURE0);
  }
}
