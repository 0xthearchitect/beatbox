/** The hero's shield: a geodesic sphere through one fixed camera, projected to
 * SVG in code, no images or model files. Y points down the screen, Z at the camera. */
type Point = readonly [number, number, number];
export interface Node { x: number; y: number; z: number }
export interface Edge { d: string; z: number }

/** viewBox units (1000 x 780). The logo is placed against these in global.css. */
export const centre = { x: 500, y: 372 };
export const radius = 292;
const PHI = (1 + Math.sqrt(5)) / 2;
// A slight tilt so the panels don't line up into a flat, symmetric badge.
const POSE: Point = [0.38, 0.52, 0.12];

function rotate([x, y, z]: Point, [rx, ry, rz]: Point): Point {
  const a = y * Math.cos(rx) - z * Math.sin(rx);
  const b = y * Math.sin(rx) + z * Math.cos(rx);
  const c = x * Math.cos(ry) + b * Math.sin(ry);
  const d = -x * Math.sin(ry) + b * Math.cos(ry);
  return [c * Math.cos(rz) - a * Math.sin(rz), c * Math.sin(rz) + a * Math.cos(rz), d];
}

const unit = ([x, y, z]: Point): Point => { const l = Math.hypot(x, y, z); return [x / l, y / l, z / l]; };
const mid = (a: Point, b: Point) => unit([(a[0] + b[0]) / 2, (a[1] + b[1]) / 2, (a[2] + b[2]) / 2]);

/** Icosahedron split once: 80 panels, big enough to read as a shield, not a ball. */
function icosphere(): Point[][] {
  const v: Point[] = [];
  for (const a of [-1, 1]) for (const b of [-PHI, PHI]) v.push([0, a, b], [a, b, 0], [b, 0, a]);
  // Every edge of this icosahedron is 2 long; anything further is not a neighbour.
  const near = (i: number, j: number) => Math.hypot(v[i]![0] - v[j]![0], v[i]![1] - v[j]![1], v[i]![2] - v[j]![2]) < 2.1;
  const faces: Point[][] = [];
  for (let i = 0; i < 12; i++) for (let j = i + 1; j < 12; j++) for (let k = j + 1; k < 12; k++) {
    if (!near(i, j) || !near(j, k) || !near(i, k)) continue;
    const [a, b, c] = [unit(v[i]!), unit(v[j]!), unit(v[k]!)];
    const ab = mid(a, b), bc = mid(b, c), ca = mid(c, a);
    faces.push([a, ab, ca], [ab, b, bc], [ca, bc, c], [ab, bc, ca]);
  }
  return faces;
}

function place(p: Point): Node {
  const [x, y, z] = rotate(p, POSE);
  const perspective = 1000 / (1000 - z * radius);
  return { x: centre.x + x * radius * perspective, y: centre.y + y * radius * perspective, z };
}

const faces = icosphere();
const key = (p: Point) => p.map((n) => n.toFixed(4)).join();
const nodes = new Map<string, Node>();
const edges = new Map<string, Edge>();
for (const face of faces) {
  face.forEach((p, i) => {
    const q = face[(i + 1) % 3]!;
    nodes.set(key(p), place(p));
    const id = [key(p), key(q)].sort().join('|');
    if (edges.has(id)) return;
    const a = place(p), b = place(q);
    edges.set(id, { d: `M${a.x.toFixed(1)} ${a.y.toFixed(1)}L${b.x.toFixed(1)} ${b.y.toFixed(1)}`, z: (a.z + b.z) / 2 });
  });
}

export const shieldNodes = [...nodes.values()];
export const shieldEdges = [...edges.values()];
/** Where the threats hit: the rim node closest to each direction (degrees, 0 = right, y down). */
export const impacts = [-142, 152, -36, 28].map((degrees) => {
  const angle = degrees * Math.PI / 180;
  const rim = shieldNodes.filter((n) => Math.abs(n.z) < 0.4);
  const score = (n: Node) => ((n.x - centre.x) * Math.cos(angle) + (n.y - centre.y) * Math.sin(angle)) / Math.hypot(n.x - centre.x, n.y - centre.y);
  const best = rim.reduce((a, b) => score(b) > score(a) ? b : a);
  const outward = Math.atan2(best.y - centre.y, best.x - centre.x);
  return { x: best.x, y: best.y, dx: Math.cos(outward), dy: Math.sin(outward) };
});
