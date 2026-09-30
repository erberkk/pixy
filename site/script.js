// Pixy's pixel face, its moods, and scroll reveals. No dependencies.

const MOODS = {
  idle: { color: '#5fd3ec', label: 'Idle', tail: '' },
  coding: { color: '#4fd68c', label: 'Coding', tail: '▮' },
  thinking: { color: '#a88bfa', label: 'Thinking', tail: '■ ■' },
  listening: { color: '#f0b44a', label: 'Listening', tail: '))' },
  alert: { color: '#ff6b7a', label: 'Needs you', tail: '!' },
};

// 14×8 sprite, redrawn from the app's own idle pose.
// a antenna · h head outline · e ears · w eyes · f face · d base shadow
const SPRITE = [
  '......a.......',
  '..............',
  '...hhhhhhhh...',
  '...hffffffh...',
  '.e.hfwwfwwh.e.',
  '...hffffffh...',
  '...hhhhhhhh...',
  '....dddddd....',
];

function shade(hex, k) {
  const n = parseInt(hex.slice(1), 16);
  const c = [n >> 16, (n >> 8) & 255, n & 255].map((v) => Math.round(k > 0 ? v + (255 - v) * k : v * (1 + k)));
  return `rgb(${c.join(',')})`;
}

function paint(svg, color) {
  const fills = { a: shade(color, 0.35), h: color, e: color, w: '#e8f7fb', f: '#0b0d14', d: shade(color, -0.45) };
  if (!svg.childElementCount) {
    const ns = 'http://www.w3.org/2000/svg';
    SPRITE.forEach((row, y) => [...row].forEach((ch, x) => {
      if (ch === '.') return;
      const r = document.createElementNS(ns, 'rect');
      r.setAttribute('x', x); r.setAttribute('y', y);
      r.setAttribute('width', 1.02); r.setAttribute('height', 1.02);
      r.dataset.k = ch;
      svg.appendChild(r);
    }));
  }
  svg.querySelectorAll('rect').forEach((r) => r.setAttribute('fill', fills[r.dataset.k]));
}

const bots = [...document.querySelectorAll('[data-bot]')];
let mood = 'idle';
function setMood(next) {
  mood = next;
  const m = MOODS[next];
  document.documentElement.style.setProperty('--mood', m.color);
  bots.forEach((b) => paint(b, m.color));
  const label = document.querySelector('[data-pill-label]');
  const tail = document.querySelector('[data-pill-tail]');
  if (label) label.textContent = m.label;
  if (tail) tail.textContent = m.tail;
  document.querySelectorAll('[data-mood]').forEach((b) => b.setAttribute('aria-checked', String(b.dataset.mood === next)));
}
setMood('idle');

document.querySelectorAll('[data-mood]').forEach((b) => b.addEventListener('click', () => setMood(b.dataset.mood)));

// Blink now and then; the eyes follow the cursor by a pixel.
const hero = document.querySelector('[data-follow]');
const reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;
if (hero && !reduced) {
  const eyes = () => hero.querySelectorAll('rect[data-k="w"]');
  const blink = () => {
    eyes().forEach((e) => e.setAttribute('height', '0.25'));
    setTimeout(() => eyes().forEach((e) => e.setAttribute('height', '1.02')), 140);
    setTimeout(blink, 2600 + Math.random() * 3200);
  };
  setTimeout(blink, 1800);
  window.addEventListener('pointermove', (ev) => {
    const r = hero.getBoundingClientRect();
    const dx = Math.max(-1, Math.min(1, (ev.clientX - (r.left + r.width / 2)) / 300));
    const dy = Math.max(-1, Math.min(1, (ev.clientY - (r.top + r.height / 2)) / 300));
    eyes().forEach((e) => { e.style.transform = `translate(${Math.round(dx) * 0.5}px, ${Math.round(dy) * 0.5}px)`; });
  }, { passive: true });
  // Clicking the robot cycles its mood.
  const order = Object.keys(MOODS);
  hero.style.cursor = 'pointer';
  hero.addEventListener('click', () => setMood(order[(order.indexOf(mood) + 1) % order.length]));
}

// Reveal on scroll
const io = new IntersectionObserver((entries) => {
  entries.forEach((e) => { if (e.isIntersecting) { e.target.classList.add('in'); io.unobserve(e.target); } });
}, { rootMargin: '0px 0px -10% 0px' });
document.querySelectorAll('.reveal').forEach((el) => io.observe(el));
