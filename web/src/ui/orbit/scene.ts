// The Orbit renderer, ported from the design handoff: the landing flight and the desk ring in WebGL2, and the still
// frames that stand in without it. Import it dynamically only; it touches the DOM only inside its functions.
import gapUrl from "./media/gap.jpg";
import { fallbackFrames, flightStops, receiptGate } from "./model";
import type { DeskModel, DeskMode } from "./model";

/** Which renderer a scene is drawing with: "still" for no WebGL2, a shader or framebuffer error, a lost context or a slow device. */
export type RendererKind = "webgl" | "still";
/** `onRenderer` is called with each change of renderer, the first one during or soon after mount. */
export type SceneOptions = { onRenderer: (kind: RendererKind) => void };
/** The landing flight. `setProgress(p)`: the scroll position through the journey, 0..1. `destroy()` releases the context. */
export type Flight = { setProgress(progress: number): void; destroy(): void };
/** A desk ring. `update(model)` as the desk's state changes; `destroy()` releases the context. */
export type Desk = { update(model: DeskModel): void; destroy(): void };

type V2 = [number, number];
type V3 = [number, number, number];
type V4 = [number, number, number, number];
type V6 = [number, number, number, number, number, number];
type Shot = { eye: V3; target: V3; center: V2; focal: number; roll: number };
type Camera = { eye: V3; right: V3; up: V3; forward: V3; center: V2; focal: number };
type Lending = { kind: number; amount: number; ltv: number; borrowing: boolean; valid: boolean; phase: number; receiptId: string | null; exchange?: V4 };

const frames = import.meta.glob<string>("./media/flight/*.webp", { eager: true, query: "?url", import: "default" });
const frameUrl = (name: string) => frames[`./media/flight/${name}.webp`]!;

const gap = .953;
const halfGap = .024;
const moonRadius = .012;
const speed = .058;
const clamp = (x: number, a: number, b: number) => Math.min(b, Math.max(a, x));
const gauss = (x: number, m: number, s: number) => Math.exp(-.5 * ((x - m) / s) ** 2);
const smooth = (a: number, b: number, x: number) => {
  const t = clamp((x - a) / (b - a), 0, 1);
  return t * t * (3 - 2 * t);
};

function profile(r: number) {
  const edge = Math.abs(r - gap) - halfGap;
  if (edge < 0) return 0;
  const envelope = smooth(.48, .58, r) * (1 - smooth(1.12, 1.26, r));
  let bands = .17 + .81 * gauss(r, .80, .083) + .47 * gauss(r, 1.045, .052) + .24 * gauss(r, .62, .030);
  bands *= .81 + .19 * Math.sin(r * 210) * Math.sin(r * 53 + 1.3);
  bands *= 1 - .60 * gauss(r, .692, .0055) - .27 * gauss(r, 1.102, .0039);
  return envelope * (bands + .30 * gauss(edge, 0, .0046));
}

function makeGrains(count: number) {
  const grains = new Float32Array(count * 8);
  let seed = 39547592;
  const random = () => {
    seed = (Math.imul(1664525, seed) + 1013904223) >>> 0;
    return seed / 4294967296;
  };
  for (let index = 0; index < count;) {
    const r = .48 + .78 * random();
    const density = profile(r);
    if (random() * 1.38 > density) continue;
    const composition = random();
    const ice = composition < .0018;
    const frost = composition < .023;
    const size = ice ? .0034 + random() ** 2 * .0058 : frost ? .0022 + random() ** 2 * .0036 : .00142 + random() ** 3 * .00234;
    grains.set([
      r, random() * Math.PI * 2,
      (random() + random() + random() - 1.5) * (frost ? .0095 : .0046),
      random(), Math.min(density, 1.2), size,
      ice ? 1.7 + random() ** 1.55 * 15.5 : random() < .29 ? .8 + random() ** 1.55 * 7.5 : 0,
      ice ? 2 : frost ? 1 : random() * .38,
    ], index++ * 8);
  }
  return grains;
}

const quad = `#version 300 es
out vec2 vUv;
void main() {
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  vUv = p;
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}`;

const cameraSource = `
uniform vec2 uRes, uCenter;
uniform vec3 uEye, uRight, uUp, uForward;
uniform float uFocal, uTime, uMoon, uPassage;
uniform vec4 uStory;
uniform vec4 uDesk, uAction;
uniform vec4 uSwap;
uniform float uSwapLock;
float gapWave(float r, float a) {
  float turn = mod(a - uMoon + 3.14159265, 6.2831853) - 3.14159265;
  float side = r - .953;
  float x = abs(turn);
  float downstream = side > 0.0 ? step(turn, 0.0) : step(0.0, turn);
  return sin(x * 53.0 - .8) * exp(-x * 3.4 - max(abs(side) - .024, 0.0) / .021)
    * smoothstep(0.0, .025, x) * downstream;
}
float edgeDrift(float ripple) {
  return ripple * (.011 + .007 * uStory.z + .018 * uDesk.y * uDesk.z);
}
float passage(float r, float a) {
  float azimuth = 2.69 - .84 * uPassage;
  float turn = mod(a - azimuth + 3.14159265, 6.2831853) - 3.14159265;
  float radial = abs(r - .924);
  return exp(-turn * turn * 18.0 - radial * radial * 480.0) * (1.0 - smoothstep(.47, 1.0, uPassage));
}`;

const worldFragment = `#version 300 es
precision highp float;
` + cameraSource + `
uniform vec3 uMoonPos;
in vec2 vUv;
out vec4 outColor;
float gauss(float x, float m, float s) {
  float a = (x - m) / s;
  return exp(-.5 * a * a);
}
float hash(vec2 p) {
  return fract(43758.5453 * sin(dot(p, vec2(127.1, 311.7))));
}
float mineralHash(vec3 p) {
  p = fract(p * vec3(.1031, .11369, .13787));
  p += dot(p, p.yzx + 19.19);
  return fract((p.x + p.y) * p.z);
}
float stone(vec3 p) {
  vec3 cell = floor(p);
  vec3 f = fract(p);
  f = f * f * (3.0 - 2.0 * f);
  float nearPlane = mix(mix(mineralHash(cell), mineralHash(cell + vec3(1, 0, 0)), f.x),
    mix(mineralHash(cell + vec3(0, 1, 0)), mineralHash(cell + vec3(1, 1, 0)), f.x), f.y);
  float farPlane = mix(mix(mineralHash(cell + vec3(0, 0, 1)), mineralHash(cell + vec3(1, 0, 1)), f.x),
    mix(mineralHash(cell + vec3(0, 1, 1)), mineralHash(cell + vec3(1, 1, 1)), f.x), f.y);
  return mix(nearPlane, farPlane, f.z);
}
vec4 moonRelief(vec3 normal) {
  vec3 surface = normal;
  float bedrock = stone(normal * 8.53);
  float fineGrain = stone(normal * 42.7 - 7.8);
  float mineral = .43 + .36 * bedrock + .17 * fineGrain;
  for (int i = 1; i <= 10; i++) {
    float seed = float(i);
    vec3 center = normalize(vec3(cos(seed * 54.91), sin(seed * 21.23), cos(seed * 76.87)));
    float size = .078 + .19 * fract(seed * .618034);
    float distanceFromCenter = length(normal - center) / size + (bedrock - .5) * .23 + (fineGrain - .5) * .19;
    float wall = gauss(distanceFromCenter, .76, .21) - gauss(distanceFromCenter, 1.07, .115) * .56;
    vec3 across = center - normal * dot(center, normal);
    surface += across * wall * (2.36 + seed * .075);
    mineral += .105 * gauss(distanceFromCenter, .99, .117) - .22 * gauss(distanceFromCenter, .37, .42);
  }
  float regolith = hash(floor(normal.xy * 441.0 + normal.z * 78.0));
  surface += .078 * (vec3(fineGrain, stone(normal * 34.13 + 15.6), stone(normal * 57.3 + 31.2)) - .5);
  return vec4(normalize(surface), clamp(mineral + regolith * .096, .22, 1.0));
}
float profile(float r, float a) {
  float displaced = r - edgeDrift(gapWave(r, a));
  float edge = abs(displaced - .953) - .024;
  if (edge <= 0.0) return 0.0;
  float envelope = smoothstep(.48, .58, r) * (1.0 - smoothstep(1.12, 1.26, r));
  float bands = .17 + .81 * gauss(displaced, .80, .083) + .47 * gauss(displaced, 1.045, .052) + .24 * gauss(displaced, .62, .030);
  bands *= .81 + .19 * sin(r * 210.0) * sin(r * 53.0 + 1.3);
  bands *= 1.0 - .60 * gauss(displaced, .692, .0055) - .27 * gauss(displaced, 1.102, .0039);
  return envelope * (bands + .30 * gauss(edge, 0.0, .0046));
}
void main() {
  vec2 screen = (gl_FragCoord.xy - uCenter) / uFocal;
  vec3 ray = normalize(uForward + screen.x * uRight + screen.y * uUp);
  vec3 color = vec3(.0010, .00092, .00134);
  vec2 stars = ray.xz * 51.0 + ray.y * vec2(32.0, -44.0);
  vec2 cell = floor(stars);
  float grain = hash(cell);
  if (grain > .987) {
    vec2 offset = fract(stars) - vec2(.28 + .52 * hash(cell + 3.7), .22 + .46 * hash(cell - 8.2));
    color += vec3(.125, .113, .103) * (1.0 - smoothstep(.006, .023, length(offset))) * (grain - .985) * 57.0;
  }
  float plane = -uEye.y / min(ray.y, -.0001);
  if (ray.y < -.0001 && plane > 0.0) {
    vec3 p = uEye + plane * ray;
    float r = length(p.xz);
    if (r > .48 && r < 1.26) {
      float a = atan(p.z, p.x);
      float density = profile(r, a);
      float lane = .77 + .13 * cos(r * 980.0 + sin(a * 10.0) * 1.5) + .10 * sin(r * 468.0 - a * .41);
      lane *= .80 + .20 * sin(r * 189.0 + .7 * sin(a * 22.0));
      float grainLight = .74 + .26 * hash(vec2(floor(r * 2350.0), floor((a - uTime * .023) * 97.0)));
      float arcLight = .42 + .58 * smoothstep(-.8, .93, dot(p.xz / r, normalize(vec2(-.46, .86))));
      arcLight *= .89 + .15 * sin(a * 13.0 + sin(r * 55.0)) * sin(a * 4.0 - r * 46.0);
      float grazing = mix(1.0, 1.62, pow(1.0 - abs(ray.y), 5.0));
      vec3 amber = mix(vec3(.62, .247, .097), vec3(1.0, .59, .28), clamp(density, 0.0, 1.0));
      color += amber * density * lane * grainLight * arcLight * grazing * .41 * (1.0 - .29 * uStory.x);
      float reflected = passage(r, a) * density * lane * (.41 + .59 * gauss(r, .923, .016));
      color += vec3(.94, .62, .30) * reflected * .34;
      if (uStory.z > .001) {
        float wake = abs(gapWave(r, a)) * gauss(abs(r - .953), .028, .012) * density;
        color += vec3(1.0, .57, .29) * wake * uStory.z * .46;
      }
      if (uSwap.x > .001 && uDesk.w > .5 && uAction.y > 1.5) {
        float landing = smoothstep(.62, .94, uAction.z);
        float selection = max(uSwap.z, uSwap.w);
        float returnAngle = uMoon + (uSwap.y < .0 ? -.67 : .86);
        float separate = abs(mod(a - returnAngle + 3.14159265, 6.2831853) - 3.14159265);
        float acceptRadius = uSwap.y < .0 ? .782 - .064 * uSwap.w : 1.029 + .094 * uSwap.w;
        float imprint = gauss(r, acceptRadius, .012 + .008 * landing) * exp(-separate * separate * 74.0);
        float lastLight = sin(landing * 3.14159265);
        color += vec3(.88, .57, .26) * imprint * (.18 + .43 * lastLight) * landing * uSwap.x * selection;
      }
      if (uAction.w > .0 && uSwap.x < .999) {
        float risk = smoothstep(.32, .92, uDesk.y) * uDesk.z;
        float lip = gauss(abs(r - .953), .028 + .019 * risk, .013);
        float azimuth = abs(mod(a - uMoon + 3.14159265, 6.2831853) - 3.14159265);
        float breadth = 1.0 - smoothstep(.42, 1.22, azimuth);
        color += mix(vec3(.85, .48, .18), vec3(1.0, .23, .10), risk) * lip * breadth * (.045 + risk * .33) * density * (1.0 - uSwap.x);
        if (uAction.y > .5 && uDesk.w > .5) {
          float lift = sin(uAction.z * 3.14159265);
          float reach = .75 + .36 * uAction.z;
          float reflection = gauss(r, reach, .024 + lift * .019) * breadth * density;
          color += vec3(1.0, .67, .26) * reflection * (uAction.y < 1.5 ? .2 : .71) * lift * (1.0 - uSwap.x);
          float credit = gauss(r, mix(.852, .986, uDesk.z), .014) * breadth * density;
          color += vec3(.94, .51, .21) * credit * (uAction.y < 1.5 ? .08 : .21) * (1.0 - uSwap.x);
          if (uAction.y > 1.5) {
            float echo = gauss(r, mix(.886, 1.009, uDesk.z), .0041) * breadth * density;
            color += vec3(1.0, .73, .37) * echo * (.28 + .43 * lift) * (1.0 - uSwap.x);
          }
        }
      }
    }
  }
  vec3 relative = uEye - uMoonPos;
  float b = dot(relative, ray);
  float hit = b * b - dot(relative, relative) + .000144;
  if (b < 0.0 && uStory.w > .0) {
    float flank = abs(sqrt(max(dot(relative, relative) - b * b, .0)) - .012);
    float corona = exp(-flank * 370.0) * exp(-pow(flank * 102.0, 2.0));
    color += vec3(.67, .33, .13) * corona * uStory.w * .57;
  }
  if (hit >= 0.0) {
    float t = -b - sqrt(hit);
    if (t > 0.0 && (ray.y >= -.0001 || t <= plane + .030)) {
      vec3 normal = normalize(relative + ray * t);
      vec3 sun = normalize(vec3(-.44, .49, -.75));
      float lit = max(dot(normal, sun), 0.0);
      float rim = pow(1.0 - max(dot(normal, -ray), 0.0), 2.8) * smoothstep(-.36, .35, dot(normal, sun));
      float mottle = .64 + .36 * hash(floor(normal.xy * 103.0 + normal.z * 23.0));
      color = vec3(.009, .012, .014) + vec3(.59, .55, .45) * (lit * .59 + rim * .47) * mottle;
      color += vec3(.91, .75, .53) * rim * pow(lit, .8) * .95;
      if (uStory.y + uStory.w > .001) {
        vec4 relief = moonRelief(normal);
        vec3 rakingLight = normalize(vec3(.59, .32, -.48));
        float sunlight = max(dot(relief.xyz, rakingLight), .0);
        float crest = smoothstep(.34, .93, sunlight) * .54;
        vec3 illuminated = vec3(.017, .014, .015) + vec3(.79, .63, .48) * relief.w * (sunlight * .68 + crest);
        float shadowRim = pow(1.0 - max(dot(normal, -ray), .0), 3.05);
        float goldenEdge = shadowRim * (.32 + .68 * max(dot(normal, normalize(vec3(-.54, .73, .15))), .0));
        color = mix(color, illuminated, uStory.y);
        color = mix(color, vec3(.0015, .0014, .0011) + vec3(1.0, .52, .21) * goldenEdge * 1.42, uStory.w);
      }
      if (uDesk.z > .001) {
        float light = max(dot(normal, normalize(vec3(.65, .37, -.42))), .0);
        float stoneShade = .65 + .35 * stone(normal * 8.0);
        vec3 warm = vec3(.018, .015, .014) + vec3(.77, .61, .40) * light * stoneShade * 1.19;
        color = mix(color, warm, uDesk.z);
      }
    }
  }
  outColor = vec4(color, 1.0);
}`;

const grainVertex = `#version 300 es
precision highp float;
layout(location = 0) in vec4 aOrbit;
layout(location = 1) in vec4 aLight;
` + cameraSource + `
uniform float uDpr, uHaze;
out vec3 vColor;
out vec2 vTangent;
out float vStretch, vPower, vDepth;
vec3 camera(vec3 p) {
  vec3 v = p - uEye;
  return vec3(dot(v, uRight), dot(v, uUp), dot(v, uForward));
}
void place(vec3 p, vec3 view, vec3 tangent, float point, float stretch) {
  vec3 ahead = camera(p + tangent);
  vec2 travel = ahead.xy / max(ahead.z, .047) - view.xy / view.z;
  vTangent = length(travel) > .00001 ? normalize(travel) : vec2(1.0, 0.0);
  vDepth = view.z;
  vStretch = stretch;
  gl_PointSize = clamp(point * stretch, 1.0, 46.0 * uDpr);
  gl_Position = vec4(view.xy * (2.0 * uFocal / uRes) + (uCenter * 2.0 / uRes - 1.0) * view.z, 0.0, view.z);
}
void braid() {
  float seed = aOrbit.w;
  float t = fract(aOrbit.y * .15915494 + uTime * .099 + seed * .13);
  float lane = floor(seed * 3.0) - 1.0;
  float shimmer = .57 + .43 * sin(seed * 71.0 + t * 19.0);
  float convergence = pow(1.0 - t, 1.55);
  float a = uMoon - .92 + t * 1.56;
  float turn = t * 13.0 + lane * 1.95 + .37 * sin(t * 4.0);
  float r = .86 + lane * .236 * convergence + .071 * sin(turn) * convergence + aOrbit.z * 3.5;
  float h = .043 + .103 * (1.0 + cos(turn)) * convergence + lane * .039 * convergence + aOrbit.z * 2.0;
  float strength = uStory.x;
  if (uStory.z > .001) {
    float driftAngle = uMoon + (t - .50) * 1.28;
    float side = sign(lane + .5 * sign(aOrbit.z));
    float wakeBase = .953 + side * (.028 + .014 * seed);
    float ripple = gapWave(wakeBase, driftAngle);
    float wakeRadius = wakeBase + ripple * .019 + aOrbit.z * 1.24;
    float wakeHeight = .012 + abs(ripple) * .033 + aOrbit.z * 3.2;
    float blend = uStory.z / max(uStory.x + uStory.z, .001);
    a = mix(a, driftAngle, blend);
    r = mix(r, wakeRadius, blend);
    h = mix(h, wakeHeight, blend);
    strength += uStory.z * (.36 + abs(ripple) * .64);
  }
  vec3 p = vec3(cos(a) * r, h, sin(a) * r);
  vec3 view = camera(p);
  if (view.z <= .047) {
    gl_Position = vec4(2.0, 2.0, 2.0, 1.0);
    gl_PointSize = 1.0;
    return;
  }
  float point = (.00075 + .0017 * pow(seed, 3.0) + uStory.x * .00065) * uFocal / view.z;
  float stretch = 1.0 + pow(shimmer, 4.0) * (3.75 + 3.5 * uStory.x);
  float envelope = smoothstep(0.0, .09, t) * (1.0 - smoothstep(.81, 1.0, t));
  float close = (t - .76) * 9.5;
  float merger = exp(-close * close) * uStory.x;
  vColor = mix(vec3(.98, .45, .19), vec3(1.0, .82, .58), clamp(step(.65, shimmer) * .85 + merger * .33, .0, 1.0));
  vPower = envelope * strength * (.26 + .84 * seed) * (1.0 + uStory.x * 1.85 + merger * 1.45) * shimmer * min(1.0, point * point * 2.0);
  place(p, view, vec3(-sin(a), .029 * sin(turn), cos(a)) * .009, point, stretch);
}
vec3 lendingSpine(float t, float lane, float lift, bool vault) {
  float turn = t * (vault ? .99 : 1.53);
  float a = uMoon + turn - (vault ? .91 : .59);
  float spread = (1.0 - t) * (1.0 - t);
  float wave = sin(t * 13.0 + lane * 1.86);
  float r;
  float h;
  if (vault) {
    float danger = smoothstep(.33, .98, uDesk.y);
    float collateral = step(2.9, abs(uAction.x));
    a += .053 * sin(t * 10.8 + lane) * sin(t * 3.14159265);
    r = .953 + (.065 + .071 * lane) * spread + (.036 + .037 * danger) * lane * sin(t * 3.14159265) - collateral * .075 * spread;
    h = .009 + (.039 + .047 * lane) * sin(t * 3.14159265) + .021 * wave * spread + lift + collateral * .028 * spread;
    h += lane * .019 * smoothstep(.64, 1.0, t);
  } else {
    r = .847 + (.38 + lane * .12) * spread + .059 * wave * spread;
    h = .009 + .13 * spread + lane * .071 * spread + .047 * wave * sin(t * 3.14159265) + lift;
  }
  return vec3(cos(a) * r, h, sin(a) * r);
}
void transfer() {
  float direction = sign(uAction.x);
  float seed = aOrbit.w;
  float t = fract(aOrbit.y * .15915494 + seed * .11);
  float extent = .49 + .51 * sqrt(uDesk.x);
  float phase = uAction.z;
  float head = mix(.1, 1.10, phase);
  float wake = (t - head) * 7.8;
  float packet = exp(-wake * wake) * (1.0 - smoothstep(.88, 1.0, phase));
  float travelled = min(.998, t + packet * .072 * (.3 + .7 * seed));
  float distance = direction < .0 ? 1.0 - travelled * extent : 1.0 - (1.0 - travelled) * extent;
  distance = clamp(distance, .0, 1.0);
  float lane = floor(seed * 3.0) - 1.0;
  float lift = aOrbit.z * (.95 + 2.1 * (1.0 - distance));
  bool vault = uDesk.z > .5;
  vec3 p = lendingSpine(distance, lane, lift, vault);
  vec3 view = camera(p);
  if (view.z <= .047) {
    gl_Position = vec4(2.0, 2.0, 2.0, 1.0);
    gl_PointSize = 1.0;
    return;
  }
  float envelope = smoothstep(.02, .14, t) * (1.0 - smoothstep(.87, 1.0, t));
  float resolved = step(.5, uAction.y) * uDesk.w;
  float receipt = step(1.5, uAction.y) * uDesk.w;
  float urgency = smoothstep(.37, .94, uDesk.y) * uDesk.z;
  vec3 source = vault ? vec3(1.0, .59, .26) : vec3(1.0, .67, .31);
  if (abs(uAction.x) >= 2.8 && direction > .0) source = vec3(.89, .82, .62);
  vColor = mix(source, vec3(1.0, .90, .67), .42 * packet + seed * .33);
  if (direction < .0) vColor = mix(vColor, vec3(1.0, .38, .15), urgency * .70);
  float point = (.00105 + .0028 * pow(seed, 2.0) + .0009 * packet) * uFocal / view.z;
  float stretch = 1.3 + 5.35 * pow(seed, 3.0) + packet * 2.1;
  float viability = mix(.44, 1.0, uDesk.w);
  float activity = .16 + 1.12 * sqrt(uDesk.x) + .37 * resolved + .29 * receipt;
  float handoff = .15 + .85 * smoothstep(.0, .25, abs(uDesk.z - .5));
  vPower = envelope * (.19 + seed * .81) * (activity * (.55 + .45 * t) + packet * (1.7 + 1.5 * receipt))
    * min(1.0, point * point * 2.4) * viability * handoff * (vault ? .55 : .96) * (1.0 - uSwap.x);
  vec3 tangent = lendingSpine(clamp(distance + direction * .006, .0, 1.0), lane, lift, vault) - p;
  place(p, view, tangent, point, stretch);
}
vec3 swapSpine(float t, float route, float seed, float locked) {
  float a = uMoon - .73 + t * 1.65 + route * .147 * sin(t * 3.14159265);
  float ramp = smoothstep(.31 + route * .24, .68 + route * .19, t);
  float crossfade = sin(ramp * 3.14159265);
  float r = mix(.782 - route * .091, 1.024 + route * .092, ramp);
  float ribbon = .010 + .021 * route;
  float h = .018 + (.027 + .081 * route) * sin(t * 3.14159265) + crossfade * ribbon;
  r += .018 * sin(t * 8.6 + route * 2.1) * (1.0 - locked * .42);
  r += seed * (.082 - locked * .037);
  h += seed * (.118 - locked * .064);
  return vec3(r * cos(a), h, r * sin(a));
}
void exchange() {
  float route = step(.5, aOrbit.w);
  float seed = fract(aOrbit.w * 2.0);
  float distance = fract(aOrbit.y * .15915494 + seed * .13);
  float direction = sign(uSwap.y);
  float t = direction < .0 ? 1.0 - distance : distance;
  float chosen = mix(uSwap.z, uSwap.w, route);
  float highlighted = max(uSwap.z, uSwap.w);
  float quotes = step(.45, uAction.y);
  float reviewed = uSwapLock * uDesk.w;
  float settled = step(1.5, uAction.y) * uDesk.w;
  float locked = reviewed * chosen;
  float amount = sqrt(uDesk.x);
  float playhead = uAction.z;
  float travel = (t - (playhead * 1.13 - .07)) * mix(8.0, 12.8, settled);
  float packet = exp(-travel * travel) * (1.0 - smoothstep(.8, 1.0, playhead));
  float limit = mix(.27 + amount * .57, 1.0, quotes);
  float envelope = smoothstep(0.0, .12, t) * (1.0 - smoothstep(limit - .08, limit + .03, t));
  float trail = mix(.62, 1.12, t);
  float depthJitter = (aOrbit.z + (seed - .5) * .002) * (1.0 + .54 * (1.0 - locked));
  vec3 p = swapSpine(distance, route, depthJitter * 12.0, locked);
  float release = smoothstep(.63, .94, playhead) * settled * chosen;
  float receipt = step(.85, fract(seed * 27.1)) * release;
  if (receipt > .0) {
    float end = direction < .0 ? .035 + distance * .10 : .965 - distance * .10;
    float scatter = (.4 + seed) * (1.0 - smoothstep(.81, 1.0, playhead));
    p = mix(p, swapSpine(end, route, depthJitter * 18.0 + scatter * .19, 1.0), receipt);
  }
  vec3 view = camera(p);
  if (view.z <= .047) {
    gl_Position = vec4(2.0, 2.0, 2.0, 1.0);
    gl_PointSize = 1.0;
    return;
  }
  float visibility = (1.0 - highlighted * .68 + chosen * .74) * (1.0 - reviewed * (1.0 - chosen) * .75);
  float valid = mix(.47, 1.0, uDesk.w);
  float glow = .38 + amount * 1.12 + quotes * .24 + locked * .39;
  float point = (.0011 + .00187 * pow(seed, 2.0) + packet * .0018 + receipt * .0009) * uFocal / view.z;
  float stretch = 1.1 + 4.1 * pow(seed, 2.0) + packet * 2.25;
  vec3 copper = mix(vec3(.98, .49, .17), vec3(1.0, .73, .38), route);
  vec3 ice = mix(vec3(.77, .83, .92), vec3(1.0, .87, .62), route);
  vColor = mix(mix(copper, ice, .5 - uSwap.y * .5), vec3(1.0, .9, .72), packet * .45 + receipt * .34);
  vPower = (envelope * trail * (glow + packet * (1.35 + settled * chosen * 2.4)) + receipt * .52)
    * visibility * valid * (.23 + seed * .77) * min(1.0, point * point * 2.3) * uSwap.x * (.34 + abs(uSwap.y) * .66);
  vec3 ahead = swapSpine(clamp(distance + direction * .007, .0, 1.0), route, depthJitter * 12.0, locked);
  place(p, view, ahead - p, point, stretch);
}
void main() {
  if (uHaze > 3.5) {
    exchange();
    return;
  }
  if (uHaze > 2.5) {
    transfer();
    return;
  }
  if (uHaze > 1.5) {
    braid();
    return;
  }
  float r = aOrbit.x;
  float velocity = .058 * pow(r, -1.5);
  float a = aOrbit.y + velocity * uTime;
  float wave = gapWave(r, a);
  float h = aOrbit.z + (.008 + .014 * uStory.z + .033 * uDesk.y * uDesk.z) * wave * sign(r - .953);
  r += edgeDrift(wave);
  vec3 p = vec3(r * cos(a), h, r * sin(a));
  vec3 view = camera(p);
  if (view.z <= .047) {
    gl_Position = vec4(2.0, 2.0, 2.0, 1.0);
    gl_PointSize = 1.0;
    return;
  }
  float point = aLight.y * uFocal / view.z;
  float stretch = 1.0 + aLight.z * mix(.46, .92, clamp(velocity * 8.0, 0.0, 1.0));
  float frost = smoothstep(.62, .86, aLight.w);
  float ice = step(1.5, aLight.w);
  float seed = aOrbit.w;
  float variance = .5 + .5 * sin(seed * 145.0 + aLight.x * 8.0);
  vec3 color = mix(vec3(1.0, .35, .14), vec3(1.0, .66, .34), .27 + variance * .68);
  color = mix(color, vec3(.97, .94, .86), frost);
  float arcLight = .58 + .42 * smoothstep(-.78, .88, dot(normalize(p.xz), normalize(vec2(-.46, .86))));
  float twinkle = pow(.5 + .5 * sin(uTime * (.55 + seed * 1.2) + seed * 170.0), 7.0);
  float power = (.18 + .42 * aLight.x * aLight.x) * (.60 + .71 * seed) * arcLight / sqrt(stretch);
  float reflected = passage(r, a);
  color = mix(color, vec3(1.0, .79, .51), reflected * .59 * (1.0 - frost));
  power *= 1.0 + .35 * abs(wave) + reflected * (.66 + frost * 1.72);
  power += frost * (.39 + 1.45 * twinkle) / sqrt(stretch);
  power += ice * (.39 + 3.8 * twinkle + reflected) / sqrt(stretch * .46);
  power *= min(1.0, point * point * 1.65) * (1.0 - .25 * uStory.x);
  if (uHaze > .5) {
    point *= 5.6;
    stretch = mix(stretch, 1.0, .76);
    power *= .037;
  }
  vColor = color;
  vPower = power;
  place(p, view, vec3(-sin(a), 0.0, cos(a)) * .008, point, stretch);
}`;

const grainFragment = `#version 300 es
precision highp float;
uniform vec4 uMoonScreen;
in vec3 vColor;
in vec2 vTangent;
in float vStretch, vPower, vDepth;
out vec4 outColor;
void main() {
  if (vDepth > uMoonScreen.z && distance(gl_FragCoord.xy, uMoonScreen.xy) < uMoonScreen.w) discard;
  vec2 p = vec2(gl_PointCoord.x, 1.0 - gl_PointCoord.y) * 2.0 - 1.0;
  float along = dot(p, vTangent);
  float across = dot(p, vec2(-vTangent.y, vTangent.x)) * vStretch;
  float distanceSquared = along * along + across * across;
  if (distanceSquared > 1.0) discard;
  float tail = mix(1.0, smoothstep(-1.0, .52, along), smoothstep(1.0, 2.2, vStretch));
  outColor = vec4(vColor * vPower * exp(-distanceSquared * 3.65) * tail, 1.0);
}`;

const glowFragment = `#version 300 es
precision mediump float;
uniform vec2 uStep;
uniform bool uExtract;
uniform sampler2D uSource;
in vec2 vUv;
out vec4 outColor;
vec3 tap(vec2 p) { return texture(uSource, vUv + p).rgb; }
void main() {
  vec3 light;
  if (uExtract) {
    light = (tap(vec2(0.0)) * 2.0 + tap(uStep) + tap(-uStep) + tap(vec2(-uStep.x, uStep.y)) + tap(vec2(uStep.x, -uStep.y))) / 6.0;
    light = max(light - .073, 0.0);
  } else {
    light = tap(vec2(0.0)) * .227;
    light += (tap(uStep * 1.385) + tap(-uStep * 1.385)) * .316;
    light += (tap(uStep * 3.231) + tap(-uStep * 3.231)) * .070;
  }
  outColor = vec4(light, 1.0);
}`;

const composeFragment = `#version 300 es
precision mediump float;
uniform sampler2D uSource, uGlow;
uniform bool uMobile;
uniform float uReframe, uLendingFrame;
in vec2 vUv;
out vec4 outColor;
void main() {
  vec3 light = texture(uSource, vUv).rgb + texture(uGlow, vUv).rgb * .76;
  float veil = uMobile ? mix(.34, 1.0, smoothstep(.20, .51, 1.0 - vUv.y)) : mix(.19, 1.0, smoothstep(.15, .58, vUv.x));
  veil = mix(veil, 1.0, uReframe * .85);
  float caption = mix(.34, 1.0, smoothstep(.0, .54, vUv.x) + (1.0 - smoothstep(.3, .81, 1.0 - vUv.y)) * .24);
  veil = mix(veil, min(caption, 1.0), uLendingFrame);
  vec3 c = 1.0 - exp(-light * 1.33 * veil);
  outColor = vec4(pow(c, vec3(.69)), 1.0);
}`;

function link<N extends string>(gl: WebGL2RenderingContext, vertex: string, fragment: string, names: readonly N[]) {
  const program = gl.createProgram();
  if (!program) throw new Error("Orbit program unavailable");
  const shaders: WebGLShader[] = [];
  try {
    for (const [type, source] of [[gl.VERTEX_SHADER, vertex], [gl.FRAGMENT_SHADER, fragment]] as const) {
      const shader = gl.createShader(type);
      if (!shader) throw new Error("Orbit shader unavailable");
      shaders.push(shader);
      gl.shaderSource(shader, source);
      gl.compileShader(shader);
      if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(shader) ?? "Orbit shader did not compile");
      gl.attachShader(program, shader);
    }
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(program) ?? "Orbit program did not link");
    return { program, uniform: Object.fromEntries(names.map((name) => [name, gl.getUniformLocation(program, name)])) as Record<N, WebGLUniformLocation | null> };
  } catch (error) {
    gl.deleteProgram(program);
    throw error;
  } finally {
    for (const shader of shaders) gl.deleteShader(shader);
  }
}

const subtract = <T extends number[]>(a: T, b: T): T => a.map((value, i) => value - b[i]!) as T;
const add = <T extends number[]>(a: T, b: T): T => a.map((value, i) => value + b[i]!) as T;
const scale = <T extends number[]>(a: T, n: number): T => a.map((value) => value * n) as T;
const dot = (a: V3, b: V3) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
const cross = (a: V3, b: V3): V3 => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
const unit = (a: V3) => scale(a, 1 / Math.max(Math.hypot(...a), .00001));
const mix = <T extends number[]>(a: T, b: T, t: number): T => a.map((value, i) => value + (b[i]! - value) * t) as T;

type Target = { texture: WebGLTexture; framebuffer: WebGLFramebuffer; w: number; h: number };
type Renderer = ReturnType<typeof createRenderer>;

function createRenderer(gl: WebGL2RenderingContext, mobile: boolean) {
  const cameraNames = ["uRes", "uCenter", "uEye", "uRight", "uUp", "uForward", "uFocal", "uTime", "uMoon", "uPassage", "uStory", "uDesk", "uAction", "uSwap", "uSwapLock"] as const;
  const scene = link(gl, quad, worldFragment, [...cameraNames, "uMoonPos"]);
  const dust = link(gl, grainVertex, grainFragment, [...cameraNames, "uDpr", "uHaze", "uMoonScreen"]);
  const glow = link(gl, quad, glowFragment, ["uSource", "uStep", "uExtract"]);
  const compose = link(gl, quad, composeFragment, ["uSource", "uGlow", "uMobile", "uReframe", "uLendingFrame"]);
  const count = mobile ? 56_000 : 104_000;
  const dustArray = gl.createVertexArray();
  const emptyArray = gl.createVertexArray();
  const buffer = gl.createBuffer();
  gl.bindVertexArray(dustArray);
  gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
  gl.bufferData(gl.ARRAY_BUFFER, makeGrains(count), gl.STATIC_DRAW);
  for (let i = 0; i < 2; i++) {
    gl.enableVertexAttribArray(i);
    gl.vertexAttribPointer(i, 4, gl.FLOAT, false, 32, i * 16);
  }
  gl.bindVertexArray(emptyArray);
  gl.useProgram(glow.program);
  gl.uniform1i(glow.uniform.uSource, 0);
  gl.useProgram(compose.program);
  gl.uniform1i(compose.uniform.uSource, 0);
  gl.uniform1i(compose.uniform.uGlow, 1);

  let targets: Target[] = [];
  let width = 0;
  let height = 0;
  let hdr = Boolean(gl.getExtension("EXT_color_buffer_float"));

  function freeTargets() {
    for (const { texture, framebuffer } of targets) {
      gl.deleteTexture(texture);
      gl.deleteFramebuffer(framebuffer);
    }
    targets = [];
  }

  function makeTarget(w: number, h: number) {
    const texture = gl.createTexture();
    const framebuffer = gl.createFramebuffer();
    if (!texture || !framebuffer) throw new Error("Orbit framebuffer unavailable");
    gl.bindTexture(gl.TEXTURE_2D, texture);
    gl.texStorage2D(gl.TEXTURE_2D, 1, hdr ? gl.RGBA16F : gl.RGBA8, w, h);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.bindFramebuffer(gl.FRAMEBUFFER, framebuffer);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, texture, 0);
    if (gl.checkFramebufferStatus(gl.FRAMEBUFFER) !== gl.FRAMEBUFFER_COMPLETE) {
      gl.deleteFramebuffer(framebuffer);
      gl.deleteTexture(texture);
      throw new Error("Orbit framebuffer unavailable");
    }
    targets.push({ texture, framebuffer, w, h });
  }

  function resize(w: number, h: number) {
    if (width === w && height === h && targets.length === 3) return;
    freeTargets();
    function allocate() {
      makeTarget(w, h);
      makeTarget(Math.ceil(w / 4), Math.ceil(h / 4));
      makeTarget(Math.ceil(w / 4), Math.ceil(h / 4));
    }
    try {
      allocate();
    } catch (error) {
      freeTargets();
      if (!hdr) throw error;
      hdr = false;
      allocate();
    }
    width = w;
    height = h;
  }

  function useCamera(pass: typeof scene | typeof dust, shot: Camera, time: number, angle: number, passage: number, story: V4, desk: V4, action: V4, swap: V6) {
    const u = pass.uniform;
    gl.useProgram(pass.program);
    gl.uniform2f(u.uRes, width, height);
    gl.uniform2fv(u.uCenter, shot.center);
    gl.uniform3fv(u.uEye, shot.eye);
    gl.uniform3fv(u.uRight, shot.right);
    gl.uniform3fv(u.uUp, shot.up);
    gl.uniform3fv(u.uForward, shot.forward);
    gl.uniform1f(u.uFocal, shot.focal);
    gl.uniform1f(u.uTime, time);
    gl.uniform1f(u.uMoon, angle);
    gl.uniform1f(u.uPassage, passage);
    gl.uniform4fv(u.uStory, story);
    gl.uniform4fv(u.uDesk, desk);
    gl.uniform4fv(u.uAction, action);
    gl.uniform4f(u.uSwap, swap[0], swap[1], swap[2], swap[3]);
    gl.uniform1f(u.uSwapLock, swap[4]);
  }

  function blur(from: Target, to: Target, x: number, y: number, extract: number) {
    gl.bindFramebuffer(gl.FRAMEBUFFER, to.framebuffer);
    gl.viewport(0, 0, to.w, to.h);
    gl.bindTexture(gl.TEXTURE_2D, from.texture);
    gl.uniform2f(glow.uniform.uStep, x, y);
    gl.uniform1i(glow.uniform.uExtract, extract);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }

  function draw(shot: Camera, time: number, angle: number, passage: number, dpr: number, narrow: boolean, story: V4, reframe: number, desk: V4, action: V4, swap: V6) {
    const world = targets[0]!;
    const a = targets[1]!;
    const b = targets[2]!;
    const moon: V3 = [Math.cos(angle) * gap, 0, Math.sin(angle) * gap];
    const displacement = subtract(moon, shot.eye);
    const moonDepth = dot(displacement, shot.forward);
    const moonFactor = shot.focal / Math.max(moonDepth, .043);
    const moonScreen: V4 = [
      shot.center[0] + dot(displacement, shot.right) * moonFactor,
      shot.center[1] + dot(displacement, shot.up) * moonFactor,
      moonDepth,
      moonDepth > 0 ? moonRadius * moonFactor : 0,
    ];
    gl.activeTexture(gl.TEXTURE0);
    gl.bindVertexArray(emptyArray);
    gl.bindFramebuffer(gl.FRAMEBUFFER, world.framebuffer);
    gl.viewport(0, 0, width, height);
    gl.disable(gl.BLEND);
    useCamera(scene, shot, time, angle, passage, story, desk, action, swap);
    gl.uniform3fv(scene.uniform.uMoonPos, moon);
    gl.drawArrays(gl.TRIANGLES, 0, 3);

    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE);
    gl.bindVertexArray(dustArray);
    useCamera(dust, shot, time, angle, passage, story, desk, action, swap);
    gl.uniform1f(dust.uniform.uDpr, dpr);
    gl.uniform4fv(dust.uniform.uMoonScreen, moonScreen);
    gl.uniform1f(dust.uniform.uHaze, 1);
    gl.drawArrays(gl.POINTS, 0, Math.floor(count / 13));
    gl.uniform1f(dust.uniform.uHaze, 0);
    gl.drawArrays(gl.POINTS, 0, count);
    if (story[0] > .001 || story[2] > .001) {
      gl.uniform1f(dust.uniform.uHaze, 2);
      gl.drawArrays(gl.POINTS, 0, Math.floor(count / 11));
    }
    if (action[3] && swap[0] < .999) {
      gl.uniform1f(dust.uniform.uHaze, 3);
      gl.drawArrays(gl.POINTS, 0, Math.floor(count / 21));
    }
    if (swap[0] > .001) {
      gl.uniform1f(dust.uniform.uHaze, 4);
      gl.drawArrays(gl.POINTS, 0, Math.floor(count / 16));
    }

    gl.disable(gl.BLEND);
    gl.bindVertexArray(emptyArray);
    gl.useProgram(glow.program);
    blur(world, a, 1.35 / width, 1.35 / height, 1);
    blur(a, b, 1.37 / a.w, 0, 0);
    blur(b, a, 0, 1.37 / b.h, 0);
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.viewport(0, 0, width, height);
    gl.useProgram(compose.program);
    gl.uniform1i(compose.uniform.uMobile, Number(narrow));
    gl.uniform1f(compose.uniform.uReframe, reframe);
    gl.uniform1f(compose.uniform.uLendingFrame, action[3]);
    gl.bindTexture(gl.TEXTURE_2D, world.texture);
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, a.texture);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    gl.activeTexture(gl.TEXTURE0);
  }

  function dispose() {
    freeTargets();
    for (const pass of [scene, dust, glow, compose]) gl.deleteProgram(pass.program);
    gl.deleteVertexArray(dustArray);
    gl.deleteVertexArray(emptyArray);
    gl.deleteBuffer(buffer);
  }
  return { resize, draw, dispose };
}

function moonSpace(angle: number) {
  const radial: V3 = [Math.cos(angle), 0, Math.sin(angle)];
  const tangent: V3 = [-radial[2], 0, radial[0]];
  const moon = scale(radial, gap);
  return (radius: number, along: number, height: number) => add(moon, add(scale(radial, radius), add(scale(tangent, along), [0, height, 0])));
}

function openingShot(narrow: boolean, opening: number, settle: boolean): Shot {
  return {
    eye: [.035 - opening * .19, 1.44 - opening * .065, 3.28 - opening * .125],
    target: [0, 0, 0],
    roll: -.23 - opening * .047,
    focal: settle ? .94 : (narrow ? 2.66 : 1.54) + opening * (narrow ? .064 : .075),
    center: settle ? [.64, .53] : narrow ? [.58 + opening * .046, .36 - opening * .014] : [.73 + opening * .031, .5 - opening * .028],
  };
}

const emptyStory = (): V4 => [0, 0, 0, 0];
const emptySwap = (): V6 => [0, 0, 0, 0, 0, 0];

function shotPath(narrow: boolean, angle: number, first: Shot, progress: number): Shot {
  const local = moonSpace(angle);
  const frames: Shot[] = [
    first,
    { eye: local(.47, -.59, .76), target: local(-.14, -.16, .030), roll: .28, focal: narrow ? 1.13 : .96, center: narrow ? [.58, .59] : [.65, .52] },
    { eye: local(.051, -.086, .071), target: local(-.002, .014, .004), roll: -.20, focal: narrow ? 1.16 : 1.12, center: narrow ? [.56, .756] : [.34, .65] },
    { eye: local(.027, -.335, .046), target: local(-.023, .065, .010), roll: .235, focal: narrow ? .97 : .85, center: narrow ? [.59, .63] : [.66, .45] },
    { eye: local(.064, .025, .034), target: local(-.005, .003, .000), roll: .105, focal: narrow ? 1.17 : .96, center: narrow ? [.53, .37] : [.66, .48] },
    { eye: [-1.39, 1.94, 3.58], target: [.005, 0, 0], roll: .19, focal: narrow ? 2.23 : 1.57, center: narrow ? [.52, .37] : [.58, .49] },
  ];
  const stops = flightStops;
  const next = Math.max(1, stops.findIndex((stop) => progress <= stop));
  const prior = next - 1;
  const span = stops[next]! - stops[prior]!;
  const fraction = clamp((progress - stops[prior]!) / span, 0, 1);
  const square = fraction * fraction;
  const cube = square * fraction;
  const sample = (read: (frame: Shot) => number) => {
    const value = read(frames[prior]!);
    const following = read(frames[next]!);
    const slope = (following - value) / span;
    const before = prior === 0 ? 0 : (value - read(frames[prior - 1]!)) / (stops[prior]! - stops[prior - 1]!);
    const after = next === frames.length - 1 ? 0 : (read(frames[next + 1]!) - following) / (stops[next + 1]! - stops[next]!);
    const tangent = (a: number, b: number) => a * b > 0 ? 2 * a * b / (a + b) : 0;
    return (2 * cube - 3 * square + 1) * value + (cube - 2 * square + fraction) * span * tangent(before, slope)
      + (-2 * cube + 3 * square) * following + (cube - square) * span * tangent(slope, after);
  };
  const vector3 = (key: "eye" | "target"): V3 => [0, 1, 2].map((index) => sample((frame) => frame[key][index]!)) as V3;
  return {
    eye: vector3("eye"), target: vector3("target"),
    center: [sample((frame) => frame.center[0]), sample((frame) => frame.center[1])],
    focal: sample((frame) => frame.focal), roll: sample((frame) => frame.roll),
  };
}

function moment(progress: number, index: number) {
  const stamp = flightStops[index]!;
  if (progress < stamp) return smooth(flightStops[index - 1]!, stamp, progress);
  return 1 - smooth(stamp, flightStops[index + 1]!, progress);
}

function lendingShot(narrow: boolean, aspect: number, angle: number, [amount, risk, vault, viable]: V4, action: V4, swap: V6): Shot {
  const local = moonSpace(angle);
  const pool: Shot = {
    eye: [.058, 1.54, 3.13], target: [0, .044, .23], center: narrow ? [.625, .52] : [.635, .53],
    focal: (narrow ? 1.51 : 1.44) + amount * .06, roll: -.32,
  };
  const debt = smooth(.34, .98, risk);
  const borrow: Shot = {
    eye: local(.25 - debt * .039, -.365, .214 - debt * .097), target: local(-.028, .031, .017),
    center: narrow ? [.62, .57] : [.63, .57], focal: narrow ? .84 : .86, roll: -.35 + debt * .41,
  };
  const approach = action[1] > .5 ? Math.sin(action[2] * Math.PI) * .024 : 0;
  const lending: Shot = {
    eye: mix(pool.eye, borrow.eye, vault), target: mix(pool.target, borrow.target, vault),
    center: mix(pool.center, borrow.center, vault), focal: pool.focal * (1 - vault) + borrow.focal * vault + approach,
    roll: pool.roll * (1 - vault) + borrow.roll * vault,
  };
  if (swap[0] < .001) return lending;

  const [blend, direction, meridian, aperture, review, release] = swap;
  const wide = smooth(2.0, 4.1, aspect);
  const lane = aperture - meridian;
  const pressure = Math.sqrt(amount) * viable;
  const lock = review * (1 - release);
  const focal = (narrow ? 1.50 : 1.43) + pressure * .27 + lock * .16 - release * .15;
  const exchange: Shot = {
    eye: [.24 + lane * .18 + direction * .04, 1.66 + wide * .12 - pressure * .17 - lock * .08 + release * .09, 3.12 - lock * .07],
    target: [.0 + lane * .083 - release * direction * .08, .030, .16 + pressure * .072 + lock * .033],
    center: [.615 - .035 * wide + lane * .037 + release * direction * .030, .61 + .175 * wide - lane * .021 + pressure * .036 + lock * .014],
    focal: focal + (1 - focal) * wide * .78 + approach * .49,
    roll: -.28 + lane * .091 + direction * .023 - lock * .026 + release * .027,
  };
  return {
    eye: mix(lending.eye, exchange.eye, blend), target: mix(lending.target, exchange.target, blend),
    center: mix(lending.center, exchange.center, blend), focal: exchange.focal * blend + lending.focal * (1 - blend),
    roll: exchange.roll * blend + lending.roll * (1 - blend),
  };
}

function makeCamera(shot: Shot, parallax: V2, width: number, height: number): Camera {
  const { target, center, focal } = shot;
  let { eye } = shot;
  const distance = Math.hypot(...subtract(target, eye));
  const aim = unit(subtract(target, eye));
  const horizon = unit(cross(aim, [0, 1, 0]));
  const lift = cross(horizon, aim);
  eye = add(eye, add(scale(horizon, parallax[0] * distance * .064), scale(lift, parallax[1] * distance * .043)));
  const forward = unit(subtract(target, eye));
  const level = unit(cross(forward, [0, 1, 0]));
  const vertical = cross(level, forward);
  const roll = shot.roll + parallax[0] * .014;
  const right = add(scale(level, Math.cos(roll)), scale(vertical, Math.sin(roll)));
  const up = add(scale(vertical, Math.cos(roll)), scale(level, -Math.sin(roll)));
  return { eye, right, up, forward, center: [center[0] * width, center[1] * height], focal: focal * width };
}

const FRAME_BUDGET_FPS = 25;
const FRAME_SAMPLE_MS = 2000;

function orbit(canvas: HTMLCanvasElement, lending: boolean, report: (kind: RendererKind) => void) {
  const settle = lending;
  let gl: WebGL2RenderingContext | null = null;
  try {
    gl = canvas.getContext("webgl2", { alpha: false, antialias: false, depth: false, powerPreference: "high-performance" });
  } catch {
    gl = null;
  }
  if (!gl) {
    report("still");
    return { setProgress() {}, setLending() {}, destroy() {} };
  }
  const context = gl;

  const media = matchMedia("(prefers-reduced-motion: reduce)");
  const host = canvas.parentElement ?? canvas;
  const listeners: (() => void)[] = [];
  let renderer: Renderer | null = null;
  let kind: RendererKind | null = null;
  let narrow = false;
  let dpr = 1;
  let resolution: V2 = [0, 0];
  let time = 0;
  let last = 0;
  let raf = 0;
  let sampled = 0;
  let sampledFrames = 0;
  let journey: number | null = null;
  let desk = emptyStory();
  let deskFrom = desk;
  let deskTo = desk;
  let swap = emptySwap();
  let swapFrom = swap;
  let swapTo = swap;
  let action: V4 = lending ? [1, 0, 1, 1] : emptyStory();
  let deskDuration = 1;
  let deskInitialized = false;
  let targetPointer: V2 = [0, 0];
  let cameraPointer: V2 = [0, 0];
  let drag: { id: number; x: number; y: number } | null = null;
  let onScreen = true;
  let degraded = false;
  let destroyed = false;

  function listen(target: EventTarget, name: string, handler: EventListener, configuration?: AddEventListenerOptions) {
    target.addEventListener(name, handler, configuration);
    listeners.push(() => target.removeEventListener(name, handler, configuration));
  }

  function setKind(next: RendererKind) {
    if (kind === next || destroyed) return;
    kind = next;
    report(next);
  }

  function stop() {
    if (raf) cancelAnimationFrame(raf);
    raf = 0;
    last = 0;
    sampled = 0;
    sampledFrames = 0;
  }

  function fallback() {
    stop();
    setKind("still");
  }

  function downgrade() {
    degraded = true;
    stop();
    renderer?.dispose();
    renderer = null;
    setKind("still");
  }

  function draw() {
    if (!renderer || context.isContextLost() || !resolution[0]) return;
    const at = journey;
    const progress = at === null || !media.matches ? at : flightStops.reduce<number>((best, stop) => Math.abs(stop - at) < Math.abs(best - at) ? stop : best, 0);
    const angle = lending ? 1.34 : progress === null ? 1.34 + time * speed * gap ** -1.5 : 1.34 + progress * .29 + Math.sin(time * .25) * .038;
    const passage = settle || media.matches ? 1 : Math.max(clamp(time / 2.15, 0, 1), smooth(0, .08, progress || 0));
    const opening = 1 - smooth(0, 1, passage);
    const first = openingShot(narrow, opening, settle);
    const story: V4 = progress === null ? emptyStory() : [moment(progress, 1), moment(progress, 2), moment(progress, 3), moment(progress, 4)];
    const reframe = progress === null ? 0 : smooth(0, .16, progress);
    const staging = lending ? lendingShot(resolution[0] / dpr < 475, resolution[0] / resolution[1], angle, desk, action, swap) : progress === null ? first : shotPath(narrow, angle, first, progress);
    const shot = makeCamera(staging, scale(cameraPointer, 1 - reframe * .78), ...resolution);
    renderer.draw(shot, time, angle, passage, dpr, narrow && !settle, story, reframe, desk, action, swap);
  }

  function resize() {
    const { width, height } = canvas.getBoundingClientRect();
    if (width < 2 || height < 2 || !renderer || context.isContextLost()) return;
    narrow = width < 690;
    const pixelLimit = narrow ? 750_000 : 1_670_000;
    dpr = Math.max(.67, Math.min(devicePixelRatio || 1, narrow ? 1.5 : 1.62, Math.sqrt(pixelLimit / (width * height))));
    resolution = [Math.round(width * dpr), Math.round(height * dpr)];
    if (canvas.width !== resolution[0]) canvas.width = resolution[0];
    if (canvas.height !== resolution[1]) canvas.height = resolution[1];
    try {
      renderer.resize(...resolution);
      draw();
      setKind("webgl");
    } catch (error) {
      console.error("Orbit scene:", error);
      fallback();
    }
  }

  function canAnimate() {
    return !destroyed && Boolean(renderer) && kind === "webgl" && !document.hidden && onScreen && !media.matches;
  }

  function active() {
    return canAnimate() && (!settle || lending && action[2] < 1);
  }

  function sync() {
    if (!active()) stop();
    else if (!raf) raf = requestAnimationFrame(frame);
  }

  function resumeVisibility() {
    if (journey !== null && onScreen && !document.hidden && !active()) draw();
    sync();
  }

  function frame(now: number) {
    raf = 0;
    if (!active()) return;
    if (last) {
      sampled += now - last;
      sampledFrames++;
      if (sampled >= FRAME_SAMPLE_MS) {
        const slow = sampledFrames * 1000 / sampled < FRAME_BUDGET_FPS;
        sampled = 0;
        sampledFrames = 0;
        if (slow) return downgrade();
      }
    }
    const delta = last ? clamp((now - last) / 1000, 0, .075) : 0;
    last = now;
    time += delta;
    if (lending && action[2] < 1) {
      action[2] = Math.min(1, action[2] + delta / deskDuration);
      desk = mix(deskFrom, deskTo, smooth(0, .65, action[2]));
      swap = mix(swapFrom, swapTo, smooth(0, .59, action[2]));
      if (swapTo[5]) swap[5] = swapFrom[5] + (1 - swapFrom[5]) * smooth(.47, .96, action[2]);
    }
    cameraPointer = mix(cameraPointer, targetPointer, 1 - Math.exp(-3.1 * delta));
    draw();
    sync();
  }

  function setProgress(value: number) {
    if (!Number.isFinite(value) || destroyed) return;
    const next = clamp(value, 0, 1);
    if (journey === next) return;
    journey = next;
    if (active()) sync();
    else if (onScreen && !document.hidden) draw();
  }

  function setLending(model: Lending, still: boolean) {
    if (!lending || destroyed) return;
    const { kind: motion, phase, amount, ltv, borrowing, valid } = model;
    const next: V4 = [amount, ltv, Number(borrowing), Number(valid)];
    deskFrom = desk;
    deskTo = next;
    swapFrom = swap;
    swapTo = model.exchange ? [...model.exchange, Number(phase >= 1), Number(phase >= 2)] : emptySwap();
    action = [motion, phase, 0, 1];
    deskDuration = model.exchange ? (phase > 1 ? 2.08 : phase > .5 ? 1.19 : .84) : (phase > 1 ? 1.75 : phase > 0 ? 1.32 : .84);
    const firstReceipt = !deskInitialized && Boolean(model.exchange) && phase > 1 && valid && !still;
    if (firstReceipt) {
      desk = deskFrom = deskTo;
      swap = swapFrom = swapTo;
    }
    if ((!deskInitialized && !firstReceipt) || !canAnimate() || still) {
      desk = deskTo;
      swap = swapTo;
      action[2] = 1;
      draw();
    } else {
      sync();
    }
    deskInitialized = true;
  }

  function resetPointer() {
    if (!drag) targetPointer = [0, 0];
  }

  function move(event: PointerEvent) {
    if (media.matches || event.pointerType === "pen") return;
    if (drag && event.pointerId === drag.id) {
      targetPointer = [clamp((event.clientX - drag.x) / 105, -1.8, 1.8), clamp((drag.y - event.clientY) / 105, -1.2, 1.2)];
    } else if (event.pointerType === "mouse") {
      const box = canvas.getBoundingClientRect();
      if (event.clientX < box.left || event.clientX > box.right || event.clientY < box.top || event.clientY > box.bottom) {
        resetPointer();
      } else {
        targetPointer = [(event.clientX - box.left) / box.width * 2 - 1, 1 - (event.clientY - box.top) / box.height * 2];
      }
    }
  }

  function startDrag(event: PointerEvent) {
    const target = event.target instanceof Element ? event.target : null;
    if (event.pointerType !== "touch" || media.matches || target?.closest("a, button, input, select, textarea, label, [role=button]")) return;
    drag = { id: event.pointerId, x: event.clientX, y: event.clientY };
  }

  function endDrag(event: PointerEvent) {
    if (drag?.id !== event.pointerId) return;
    drag = null;
    resetPointer();
  }

  function initialize() {
    if (degraded || destroyed) return;
    try {
      renderer = createRenderer(context, canvas.getBoundingClientRect().width < 690);
      resize();
      sync();
    } catch (error) {
      console.error("Orbit scene:", error);
      renderer = null;
      fallback();
    }
  }

  listen(canvas, "webglcontextlost", (event) => {
    event.preventDefault();
    renderer = null;
    fallback();
  });
  listen(canvas, "webglcontextrestored", initialize);
  listen(document, "visibilitychange", resumeVisibility);
  listen(media, "change", () => {
    if (media.matches) {
      targetPointer = [0, 0];
      cameraPointer = targetPointer;
      if (lending) {
        desk = deskTo;
        swap = swapTo;
        action[2] = 1;
      }
    }
    draw();
    sync();
  });
  const priorTouchAction = host.style.touchAction;
  if (!settle) {
    listen(window, "pointermove", move as EventListener, { passive: true });
    listen(window, "pointerup", endDrag as EventListener, { passive: true });
    listen(window, "pointercancel", endDrag as EventListener, { passive: true });
    listen(window, "blur", resetPointer);
    listen(host, "pointerleave", resetPointer, { passive: true });
    listen(host, "pointerdown", startDrag as EventListener, { passive: true });
    if (!priorTouchAction) host.style.touchAction = "pan-y pinch-zoom";
  }
  const observer = new ResizeObserver(() => {
    resize();
    sync();
  });
  observer.observe(canvas);
  const visibility = new IntersectionObserver(([entry]) => {
    onScreen = entry?.isIntersecting ?? onScreen;
    resumeVisibility();
  });
  visibility.observe(canvas);
  initialize();

  function destroy() {
    if (destroyed) return;
    destroyed = true;
    stop();
    observer.disconnect();
    visibility.disconnect();
    for (const off of listeners) off();
    if (!settle && !priorTouchAction && host.style.touchAction === "pan-y pinch-zoom") host.style.touchAction = priorTouchAction;
    renderer?.dispose();
    renderer = null;
    context.getExtension("WEBGL_lose_context")?.loseContext();
  }

  return { setProgress, setLending, destroy };
}

function overlay(canvas: HTMLCanvasElement, css: string) {
  const layer = document.createElement("div");
  layer.setAttribute("aria-hidden", "true");
  layer.style.cssText = "position:absolute;inset:0;pointer-events:none;" + css;
  canvas.after(layer);
  return layer;
}

function flightFrames(canvas: HTMLCanvasElement) {
  const reduced = matchMedia("(prefers-reduced-motion: reduce)");
  const portrait = matchMedia("(max-width: 689px)");
  const images = new Map<string, HTMLImageElement>();
  let layer: HTMLDivElement | null = null;
  let progress: number | null = null;

  function frame(name: string) {
    const known = images.get(name);
    if (known) return known;
    const image = new Image();
    image.alt = "";
    image.style.cssText = "position:absolute;inset:0;width:100%;height:100%;object-fit:cover;pointer-events:none;opacity:0";
    image.onload = render;
    layer!.append(image);
    images.set(name, image);
    image.src = frameUrl(name);
    return image;
  }

  function render() {
    if (!layer || progress === null) return;
    const { from, to, mix: blend } = fallbackFrames(progress, portrait.matches);
    let fade = reduced.matches ? (blend < .5 ? 0 : 1) : blend;
    const left = frame(from);
    const right = fade === 0 ? left : frame(to);
    if (!(left.complete && left.naturalWidth)) return;
    if (!(right.complete && right.naturalWidth)) fade = 0;
    for (const image of images.values()) {
      image.style.opacity = image === left ? "1" : image === right ? String(fade) : "0";
      image.style.zIndex = image === right && right !== left ? "1" : "0";
    }
  }

  function show(on: boolean) {
    if (!on) {
      layer?.remove();
      layer = null;
      images.clear();
      return;
    }
    layer ??= overlay(canvas, `background:#030302 url("${gapUrl}") 57% 50% / cover no-repeat`);
    render();
  }

  reduced.addEventListener("change", render);
  portrait.addEventListener("change", render);
  return {
    show,
    setProgress(value: number) {
      if (!Number.isFinite(value)) return;
      progress = clamp(value, 0, 1);
      render();
    },
    destroy() {
      reduced.removeEventListener("change", render);
      portrait.removeEventListener("change", render);
      show(false);
    },
  };
}

/** Mounts the landing flight on `canvas`, whose parent must be a positioned box: the still frames are drawn in a layer after the canvas. */
export function mountFlight(canvas: HTMLCanvasElement, { onRenderer }: SceneOptions): Flight {
  const still = flightFrames(canvas);
  const engine = orbit(canvas, false, (kind) => {
    still.show(kind === "still");
    onRenderer(kind);
  });
  return {
    setProgress(progress) {
      engine.setProgress(progress);
      still.setProgress(progress);
    },
    destroy() {
      engine.destroy();
      still.destroy();
    },
  };
}

const kinds: Record<DeskMode, number> = { supply: 1, redeem: -1, add: 2, draw: -2, repay: 3, remove: -3, close: 4 };
const initial: Lending = { kind: 1, amount: 0, ltv: 0, borrowing: false, valid: false, phase: 0, receiptId: null };
type Route = [V2, V2, V2, V2];
const quoteRoutes: Route[] = [
  [[649, 310], [548, 335], [422, 404], [278, 359]],
  [[665, 354], [573, 408], [420, 459], [272, 388]],
];
const receipts = receiptGate();

function within(value: number, limit: number) {
  return Number.isFinite(value) ? Math.max(0, Math.min(limit, value)) : 0;
}

function normalize(model: DeskModel): Lending {
  const receiptId = model.receiptId ?? null;
  if (model.tab === "swap") {
    return {
      kind: 1, amount: within(model.fraction, 1), ltv: 0, borrowing: false,
      valid: model.valid,
      phase: model.phase === "settled" ? 2 : model.phase === "review" ? 1 : model.phase === "quotes" ? .5 : 0,
      receiptId,
      exchange: [1, model.sell === "quote" ? -1 : 1, Number(model.lane === 0), Number(model.lane === 1)],
    };
  }
  const borrowing = model.tab === "borrow";
  return {
    kind: kinds[model.mode],
    amount: within(model.fraction, 1),
    ltv: borrowing ? within(model.ltv ?? 0, 1.25) : 0,
    borrowing,
    valid: model.valid,
    phase: model.phase === "settled" ? 2 : model.phase === "review" ? 1 : 0,
    receiptId,
  };
}

function randomSource(seed: number) {
  return () => {
    seed = (Math.imul(1664525, seed) + 1013904223) >>> 0;
    return seed / 4294967296;
  };
}

function spot(x: number, y: number, size: number, opacity: number, color: string) {
  return '<circle cx="' + x.toFixed(1) + '" cy="' + y.toFixed(1) + '" r="' + size.toFixed(2) + '" fill="' + color + '" opacity="' + opacity.toFixed(2) + '"/>';
}

const opening = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 720 450" preserveAspectRatio="xMidYMid slice">';
const definitions = '<defs><linearGradient id="caption" x1="0" y1="1" x2=".65" y2=".16"><stop stop-color="#060504" stop-opacity=".90"/><stop offset=".75" stop-color="#060504" stop-opacity="0"/></linearGradient><filter id="heat"><feGaussianBlur stdDeviation="7"/></filter></defs>';

function stream(model: Lending) {
  const random = randomSource(656802);
  const vault = model.borrowing;
  const bias = model.kind < 0 ? -1 : 1;
  const phase = model.phase * Number(model.valid);
  const intensity = (.23 + Math.sqrt(model.amount) * .72) * (model.valid ? 1 : .63);
  const extent = .49 + .51 * Math.sqrt(model.amount);
  const warmth = model.ltv > .67 ? "#f48453" : model.kind >= 3 ? "#f5dca8" : "#efb468";
  let transfer = "";
  for (let lane = -1; lane <= 1; lane++) {
    for (let index = 0; index < 85; index++) {
      const cursor = (index + random()) / 85;
      const s = bias > 0 ? 1 - (1 - cursor) * extent : 1 - cursor * extent;
      const spread = (1 - s) ** 1.75;
      const x = (vault ? 715 : 758) + (vault ? -254 : -222) * s + lane * (vault ? -22 : 48) * spread;
      const crest = vault ? 53 + model.ltv * 41 : 54;
      const y = (vault ? 259 : 320) + (vault ? 12 : 60) * s + lane * crest * spread + 16 * Math.sin(s * 10 + lane * 1.5) * spread;
      const power = intensity * (.32 + random() * .62) * (.45 + cursor * .55) * (.92 + phase * .12);
      const jitter = (random() - .5) * (4 + spread * 8);
      transfer += spot(x + jitter, y + jitter, .44 + random() ** 4 * (1.35 + model.amount * .60), Math.min(power, .99), warmth);
    }
  }
  if (phase > 0) {
    const x = vault ? 461 : 536;
    const y = vault ? 271 : 380;
    const radius = (phase > 1 ? 24 : 14) + 12 * model.amount;
    transfer += '<circle cx="' + x + '" cy="' + y + '" r="' + radius + '" fill="none" stroke="#eaaa61" stroke-opacity=".45" stroke-width=".8"/>';
    transfer += spot(x, y, phase > 1 ? 3.1 : 1.6, .94, "#ffd296");
  }
  return transfer;
}

function coordinates([start, rise, fall, end]: Route, t: number) {
  const left = 1 - t;
  return start.map((axis, i) => left ** 3 * axis + 3 * t * left ** 2 * rise[i]! + 3 * t ** 2 * left * fall[i]! + t ** 3 * end[i]!) as V2;
}

function path([start, rise, fall, end]: Route) {
  return "M" + start.join(" ") + "C" + [rise, fall, end].map((point) => point.join(" ")).join(" ");
}

function exchange(model: Lending & { exchange: V4 }, animate: number) {
  const [, direction, meridian, aperture] = model.exchange;
  const amount = Math.sqrt(model.amount);
  const choices = [meridian, aperture];
  const chosen = Math.max(...choices);
  const tint = direction > 0 ? ["#ffb874", "#e6a761"] : ["#cfdded", "#ffe0ae"];
  const active = model.valid ? 1 : .48;
  const limit = model.phase >= .5 ? 1 : .31 + amount * .61;
  const pace = model.phase === 2 ? 1710 : model.phase === 1 ? 1000 : 735;
  let drawing = "";
  for (const [index, route] of quoteRoutes.entries()) {
    const points = direction > 0 ? route : (route.slice().reverse() as Route);
    const random = randomSource(760126 + index * 79);
    const lock = model.phase >= 1 ? .19 : .50;
    const choice = choices[index]!;
    const emphasis = (1 - chosen * (1 - choice) * (1 - lock)) * active;
    const color = tint[index]!;
    const routePath = path(points);
    if (model.phase >= .5) {
      drawing += '<path d="' + routePath + '" fill="none" stroke="' + color + '" stroke-width=".81" stroke-opacity="' + (.26 * emphasis).toFixed(2) + '"/>';
    }
    for (let grain = 0; grain < 87; grain++) {
      const t = (grain + random()) / 87 * limit;
      const point = coordinates(points, t);
      const swell = (1 - .35 * choice * Number(model.phase >= 1)) * (2 + 6 * (1 - t));
      const x = point[0] + (random() - .5) * swell;
      const y = point[1] + (random() - .5) * swell;
      const light = (.20 + random() * .80) * (.32 + amount * .58) * (.48 + t * .52) * emphasis;
      drawing += spot(x, y, .34 + random() ** 2 * (1.30 + amount * .73), light, color);
    }
    if (animate && (!chosen || choice) && model.valid) {
      const duration = pace + index * 73;
      drawing += '<g opacity="0"><animateMotion dur="' + duration + 'ms" rotate="auto" path="' + routePath + '"/><animate attributeName="opacity" values="0;.83;.87;0" keyTimes="0;.18;.78;1" dur="' + duration + 'ms"/>';
      drawing += spot(-6, .4, 1.6, .56, color) + spot(0, 0, 2 + amount * 2, .95, "#ffdfaa") + spot(6, -.9, 1.1, .83, color) + "</g>";
    }
    if (model.phase === 2 && model.valid && choice) {
      const [x, y] = points[3];
      if (animate) {
        drawing += '<circle cx="' + x + '" cy="' + y + '" r="2" fill="none" stroke="' + color + '" stroke-width="1.1" opacity="0"><animate attributeName="opacity" values=".85;0" begin="1.45s" dur=".48s"/><animate attributeName="r" from="2" to="21" begin="1.45s" dur=".48s"/></circle>';
      }
      drawing += '<circle cx="' + x + '" cy="' + y + '" r="10" fill="none" stroke="' + color + '" stroke-width=".7" opacity=".55"/>';
      for (let spark = 0; spark < 22; spark++) {
        const angle = random() * Math.PI * 2;
        const radius = random() * 15;
        drawing += spot(x + Math.cos(angle) * radius, y + Math.sin(angle) * radius, .38 + random() * 1.3, .22 + random() * .56, color);
      }
    }
  }
  return drawing;
}

function svgImage(content: string, pass: number) {
  const start = pass ? opening.replace("<svg ", '<svg data-pass="' + pass + '" ') : opening;
  return 'url("data:image/svg+xml,' + encodeURIComponent(start + content + "</svg>") + '")';
}

function image(model: Lending, pass: number) {
  const danger = Math.max(0, Math.min(1, (model.ltv - .34) / .64));
  let content = definitions;
  if (model.borrowing) {
    const crest = 254 - danger * 15;
    const haze = .03 + danger * .30;
    content += '<path d="M379 225Q401 ' + crest + ' 438 291T724 422" fill="none" stroke="#d16337" stroke-width="' + (3 + danger * 19) + '" stroke-opacity="' + haze + '" filter="url(#heat)"/>';
  }
  content += model.exchange ? exchange({ ...model, exchange: model.exchange }, pass) : stream(model);
  content += '<path fill="url(#caption)" d="M0 0h720v450H0z"/>';
  return svgImage(content, pass) + ', url("' + frameUrl("wide-" + (model.borrowing ? 3 : 0)) + '")';
}

/**
 * Mounts a desk ring on `canvas`, whose parent must be a positioned box: without WebGL the illustrated stills are drawn
 * in a layer after the canvas. A receipt animates once per page, however often the desk remounts or the model repeats.
 */
export function mountDesk(canvas: HTMLCanvasElement, { onRenderer }: SceneOptions): Desk {
  const motion = matchMedia("(prefers-reduced-motion: reduce)");
  let current = initial;
  let serialized = "";
  let passes = 0;
  let kind: RendererKind | null = null;
  let layer: HTMLDivElement | null = null;
  let destroyed = false;

  function apply(animate = false) {
    if (destroyed) return;
    if (kind !== "still") {
      layer?.remove();
      layer = null;
      return;
    }
    layer ??= overlay(canvas, "background-color:#080705;background-repeat:no-repeat;background-size:cover");
    const pass = current.exchange && animate && !motion.matches ? ++passes : 0;
    layer.style.backgroundImage = image(current, pass);
    layer.style.backgroundPosition = current.exchange ? "center 76%" : "center";
  }

  const engine = orbit(canvas, true, (next) => {
    kind = next;
    apply();
    onRenderer(next);
  });

  const refresh = () => apply();
  motion.addEventListener("change", refresh);
  apply();

  return {
    update(model) {
      if (destroyed) return;
      const next = normalize(model);
      const key = JSON.stringify(next);
      if (key === serialized) return;
      const still = next.phase === 2 && next.valid && (!next.receiptId || !receipts.claim(next.receiptId));
      current = next;
      serialized = key;
      engine.setLending(next, still);
      apply(!still);
    },
    destroy() {
      if (destroyed) return;
      destroyed = true;
      motion.removeEventListener("change", refresh);
      engine.destroy();
      layer?.remove();
      layer = null;
    },
  };
}
